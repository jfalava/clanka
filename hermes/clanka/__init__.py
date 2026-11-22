"""clanka for Hermes Agent: long-term memory for one companion.

A general plugin on Hermes's public plugin API only:

- ``pre_llm_call``: checks for the doctor token (the turn is blocked and memory
  frozen), blocks every turn while memory is frozen or cannot be opened, then
  records the user's message and adds the turn's memory, mood and notes as
  context. A photo is recorded as ``*shows you a photo*``, not as the notes and
  paths Hermes writes about it for the model.
- ``transform_llm_output``: takes the em dashes out of the reply before Hermes
  keeps it, so the model's own history stops teaching it the next one. The
  reply is not held back for this: on a surface that streams, the user sees it
  as written and then as kept. Text written before a tool call never reaches
  this hook; clanka records it without them all the same.
- ``post_llm_call``: records the reply, text written before a tool call
  included, then reflects on the exchange on a background thread. Reflection
  runs on ``ctx.llm``, the user's active model, so the companion's memory is
  written by the same model it chats with.
- Tools ``recall_memory``, ``go_to_sleep`` and, in a dream, ``wake_up``. The
  night (or the dream) ends as the reply is recorded, so the next message
  starts a new conversation at once; sleep or waking consolidates it behind,
  on a background thread, and posts one line to the chat saying it happened,
  in counts only (``ctx.post_message``: not kept in Hermes's transcript, never
  shown to the model).
- ``~~`` mid-conversation ends the conversation and starts the dream; the
  conversation is slept on behind it.
- ``zz`` at the start of a message says goodnight for sure: the companion
  answers what follows it as the conversation's last beat and sleeps after it,
  tool call or not (in a dream, it wakes). ``/sleep [words]`` sends that message for the user, which
  needs ``plugins.entries.clanka.allow_gateway_injection`` (``clanka hermes
  profile`` sets it).
- ``/triage`` answers with the companion's state and its last diagnostics
  events, counts and fixed words only; no turn, no model call.
- ``/lore`` lists the lore entries the companion may read, ``/lore <id>``
  shows one as the companion reads it (and opens it for the conversation),
  and ``/lore strike <id> <line>`` hides a companion's line from every
  reader. The ``read_lore`` tool lets the model open an entry itself; one
  the user names opens on its own.
- ``on_session_reset`` (``/new``, ``/reset``, a session-reset policy plugin):
  the companion sleeps on whatever was said since it last slept, so a
  conversation the user walked away from still becomes a memory. Nothing
  happens when nothing was said.
- ``on_session_finalize``: the same when someone leaves the chat the
  companion is talking in (quitting the CLI, a one-shot exiting, the TUI), so
  that conversation doesn't stay open and run into the next one. Ends Hermes
  makes on its own and keeps resumable, and other chats ending, leave it
  awake. ``plugins.entries.clanka.settings.sleep_on_leave: false`` makes
  leaving keep it awake too, for a script that sends each message as a
  one-shot (``hermes/bench.sh``); ``/new`` still puts it to sleep.
- ``plugins.entries.clanka.settings.session_tokens`` sets how much
  conversation this companion holds before it tires, in estimated tokens;
  it wins over ``[session] tokens`` in clanka's config.toml.

The companion is the soul.md named by ``plugins.entries.clanka.settings.soul``
in the profile's config.yaml (``$CLANKA_SOUL`` as a fallback). Put its persona in
Hermes's SOUL.md with ``clanka soul persona <soul.md>``; never copy the whole
file, whose frontmatter holds the doctor token.
"""

from __future__ import annotations

import concurrent.futures
import contextvars
import json
import logging
import os
import re
import threading

logger = logging.getLogger(__name__)

# Most one memory-writing reply may run to.
MAX_TOKENS = 2048
# Added after a ``zz`` goodnight's context, last, so the model calls the tool
# the user asked for. Sleep comes after the reply whether it does or not.
GO_TO_SLEEP = "Call go_to_sleep first, then write that last reply."

# How long /triage waits for memory to open: a command answers at once or not
# at all, and an open that waits on a keychain prompt can take minutes.
TRIAGE_TIMEOUT = 5

# Longest a turn waits for memory to open, in seconds. Hermes gives a hook
# callback 30s, then drops it and lets the model answer with no memory, so the
# turn has to be blocked before that.
OPEN_TIMEOUT = 20

# The finalize reasons that mean someone ended the conversation: a viewer
# leaving it (cli_close, tui_close) or /new. Hermes also finalizes chats nobody
# ended and that it keeps resumable (a dropped socket reaped, a gateway
# stopping or restarting, an idle or evicted TUI session with no reason given,
# a CLI viewer resuming another chat), and sleep can't be taken back, so only
# these sleep. A CLI quit without the gateway is "shutdown" on platform "cli".
DELIBERATE_ENDS = frozenset({"cli_close", "tui_close", "new_session"})


PHOTO = "*shows you a photo*"
# The desktop app's line for an attached image in the message it keeps.
_IMAGE_LINE = re.compile(r"^@image:.*$\n?", re.MULTILINE)


def _images(content):
    """How many image parts a message's content holds."""
    if not isinstance(content, list):
        return 0
    return sum(isinstance(p, dict) and "image" in str(p.get("type")) for p in content)


def _heard(message, history=None):
    """What the user said in a turn's message, as memory keeps it. Hermes hands
    over a photo as content parts, as notes about it for the model (a
    description, a path to look at, ``@image:`` lines, a caption it made up when
    there was none), or not at all (the CLI passes the typed text, and the photo
    is only in the turn's own message, the last in ``history``); each photo
    becomes ``PHOTO`` after the words, which keeps the markers a message starts
    with (``zz``, ``~~``) at its start."""
    last = history[-1] if history else None
    photos = _images(last.get("content")) if isinstance(last, dict) and last.get("role") == "user" else 0
    if isinstance(message, list):
        photos = max(photos, _images(message))
        message = "\n".join(
            p.get("text") or "" for p in message if isinstance(p, dict) and "text" in str(p.get("type"))
        )
    text = message or ""
    try:
        from agent.image_routing import split_image_notes

        text, paths = split_image_notes(text)
        photos = max(photos, len(paths))
    except Exception:
        logger.debug("clanka: Hermes's image notes are not readable here", exc_info=True)
    photos = max(photos, len(_IMAGE_LINE.findall(text)))
    text = _IMAGE_LINE.sub("", text).strip()
    return " ".join([text] * bool(text) + [PHOTO] * photos)


# What a notice starts with: a line the plugin shows in place of a reply, when
# it blocks a turn. Hermes keeps that line in the conversation as the
# assistant's, so the model reads it back as something it said.
NOTICE = "clanka: "

# Told to the reply model on the turn after a notice.
NOTICE_HINT = (
    "Lines above that start with `clanka:` are notices from your memory, shown in place of a "
    "reply: the message before each one never reached you. You did not say them, so never "
    "mention, explain or apologise for them."
)

# What the user is told when memory cannot take a turn, by the kind of error
# (the word `clanka diagnostics` counts). Never the error's own text: it names
# files on the machine Hermes runs on, and a notice goes into the chat.
_WHY = {
    "persona_unresolved": "The soul.md changed since these memories were written, so nothing "
    "was sent. Run `clanka soul continue`, `update` or `fork` on it, then say that again.",
    "lore_invalid": "The lore this companion reads has errors, so nothing was sent. Run "
    "`clanka lore lint`, fix what it lists, then say that again.",
    "database_gone": "The memory's database was deleted or replaced while it was open, so "
    "nothing was sent. Put it back, or restart Hermes to start with an empty memory.",
}


def _block(message):
    return {"action": "block", "message": NOTICE + message}


def _why(error):
    kind = getattr(error, "code", "other")
    return _WHY.get(kind) or (
        f"Memory is not working, so nothing was sent ({kind}). Hermes's log has the whole error."
    )


def _no_triage(error):
    # The kind only, as in _why: the error's text can name files on this machine.
    kind = getattr(error, "code", "other")
    return f"no triage: memory is not working ({kind}). Hermes's log has the whole error."


def _text(content):
    """A message's text, whether Hermes holds it as a string or as parts."""
    if isinstance(content, list):
        return "\n".join(str(p.get("text", "")) for p in content if isinstance(p, dict))
    return str(content or "")


def _notices(history):
    """For each reply before this turn, oldest first, whether it was a notice.
    ``history`` ends with this turn's own message."""
    return [
        _text(m.get("content")).startswith(NOTICE)
        for m in (history or [])[:-1]
        if isinstance(m, dict) and m.get("role") == "assistant"
    ]


def _said(history, final):
    """The whole reply, as the user saw it. Hermes passes ``post_llm_call`` only
    the text after the last tool call; what the model wrote before a call (a
    dream's last moment before ``wake_up``) is on this turn's assistant
    messages that made the calls, which the gateway shows too."""
    before = []
    for m in reversed(history or []):
        role = m.get("role") if isinstance(m, dict) else None
        if role == "user":
            break
        if role == "assistant" and m.get("tool_calls"):
            before.append(_text(m.get("content")).strip())
    parts = [*reversed(before), (final or "").strip()]
    return "\n\n".join(p for p in parts if p)


def _dashless(response_text="", **_):
    from . import _clanka

    plain = _clanka.without_dashes(response_text or "")
    return plain if plain != response_text else None


class _Plugin:
    def __init__(self, ctx):
        self._ctx = ctx
        self._opening = None
        self._open_lock = threading.Lock()
        # Reflections and sleep run one at a time, in order, off the turn thread.
        self._work_lock = threading.Lock()
        self._sleep_requested = False
        # The Hermes chat the companion's last turn came from: the one its
        # open conversation is in.
        self._live_chat = None

    # The companion opens on first use, so a slow Secret Service or model load
    # never counts against Hermes's plugin load timeout. It opens on a thread
    # of its own: a caller can stop waiting (macOS asks on screen before a new
    # program may read the key, and nobody may be there) while the open carries
    # on, and the next caller waits on the same attempt. One that failed is
    # tried again by the next caller.
    def companion(self, timeout=None):
        with self._open_lock:
            opening = self._opening
            if opening is None or (opening.done() and opening.exception() is not None):
                soul = self._soul()
                if not soul:
                    raise RuntimeError(
                        "set plugins.entries.clanka.settings.soul to the companion's soul.md"
                    )
                opening = self._opening = concurrent.futures.Future()
                threading.Thread(
                    target=self._open, args=(opening, soul), name="clanka-open", daemon=True
                ).start()
        return opening.result(timeout)

    def _soul(self):
        # Profile config first: under Hermes's multiplexed gateway a profile's
        # .env doesn't reach a plugin's os.environ.
        return self._ctx.get_config("soul") or os.environ.get("CLANKA_SOUL")

    def _session_tokens(self):
        """This profile's conversation allowance, which wins over ``[session]
        tokens`` in clanka's config.toml; ``None`` when the profile sets none."""
        tokens = self._ctx.get_config("session_tokens")
        if tokens in (None, ""):
            return None
        try:
            return int(tokens)
        except (TypeError, ValueError):
            logger.warning("clanka: session_tokens %r is not a number; ignored", tokens)
            return None

    def _note(self, what):
        """Notes what the core can't see in clanka's diagnostics log (``clanka
        diagnostics``), for this companion: one of a few fixed words, never
        anything that was said."""
        try:
            from . import _clanka

            _clanka.note(what, self._soul())
        except Exception:
            logger.debug("clanka: diagnostics did not take %r", what, exc_info=True)

    def _open(self, opening, soul):
        try:
            from . import _clanka

            opening.set_result(
                _clanka.Companion(soul, self._model, session_tokens=self._session_tokens())
            )
        except BaseException as e:
            opening.set_exception(e)

    def _model(self, system, messages, temperature):
        """The host's active model, for the core's memory writing."""
        # The system prompt (persona, instructions, tools) is the same on every
        # turn and everything about this exchange is in the messages, so it is
        # marked for the provider's prompt cache. Hermes leaves a one-shot call
        # uncached unless the caller marks it, and drops the marker on a
        # provider without such a cache.
        prefix = {
            "role": "system",
            "content": [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}],
        }
        result = self._ctx.llm.complete(
            [prefix, *messages],
            temperature=temperature,
            max_tokens=MAX_TOKENS,
            purpose="clanka.memory",
        )
        # Hermes reports a provider's cache reads and writes to a plugin only
        # on some routes; on Anthropic they are missing and the cached prefix
        # is simply absent from the input count, so that count is what to watch.
        u = result.usage
        logger.info(
            "clanka: memory model call: %d chars in %d messages; %d tokens in, %d out%s",
            len(system) + sum(len(m["content"]) for m in messages),
            len(messages),
            u.input_tokens,
            u.output_tokens,
            f" ({u.cache_read_tokens} read from cache, {u.cache_write_tokens} written)"
            if u.cache_read_tokens or u.cache_write_tokens
            else "",
        )
        logger.debug("clanka: memory model reply: %s", result.text)
        return result.text

    def pre_llm_call(
        self, user_message="", is_first_turn=True, conversation_history=None, session_id=None, **_
    ):
        # Hermes replays injected context on every later turn, so after the
        # first one only what the model hasn't seen is injected, and nothing
        # at all when nothing is new.
        #
        # Hermes runs the turn anyway when this hook raises or outlasts its
        # time limit, and the reply would be one the companion never
        # remembers. So a memory that cannot take the turn blocks it instead.
        #
        # A conversation whose every turn so far was blocked has shown the
        # model nothing, though Hermes no longer calls this its first turn.
        notices = _notices(conversation_history)
        first = bool(is_first_turn) or (bool(notices) and all(notices))
        try:
            companion = self.companion(OPEN_TIMEOUT)
            kind, context = companion.begin_turn(
                _heard(user_message, conversation_history), first
            )
        except concurrent.futures.TimeoutError:
            logger.warning("clanka: memory did not open in %ds; the turn is blocked", OPEN_TIMEOUT)
            self._note("open_timeout")
            return _block(
                "Memory has not opened yet, so nothing was sent. On a Mac, choose Always Allow "
                "on the keychain prompt on its screen, then say that again."
            )
        except Exception as e:
            logger.exception("clanka: memory cannot take the turn; the turn is blocked")
            self._note("turn_blocked")
            return _block(_why(e))
        if kind == "doctor":
            return _block("Memory frozen.")
        if kind == "frozen":
            return _block(
                "Memory is frozen by the doctor. `clanka doctor unfreeze` lets us talk again."
            )
        self._live_chat = session_id
        parts = [context] if context else []
        # Once, on the turn after a notice: Hermes replays it from then on.
        if notices and notices[-1]:
            parts.append(NOTICE_HINT)
        if kind == "goodnight":
            parts.append(GO_TO_SLEEP)
        return {"context": "\n\n".join(parts)} if parts else None

    def api_request_error(self, **_):
        # A request to the reply model failed. Hermes may try again, so one
        # turn can note several; a turn that got no reply at all also shows on
        # the next one, as `unanswered`.
        self._note("model_failed")

    def post_llm_call(self, assistant_response="", conversation_history=None, **_):
        # Hermes calls this for a blocked turn too. One that pre_llm_call
        # blocked because memory did not open has nothing to record, and
        # waiting for the open here would hold the blocked reply back until
        # Hermes drops this hook. A turn that asked for sleep got past
        # pre_llm_call, so memory is open and the request is not lost here.
        opening = self._opening
        if opening is None or not opening.done() or opening.exception() is not None:
            return
        companion = opening.result()
        ended = companion.end_turn(_said(conversation_history, assistant_response))
        asked, self._sleep_requested = self._sleep_requested, False
        turns, consolidate = None, False
        if ended is not None:
            user, reply, consolidate = ended
            turns = (user, reply)
        # The model called go_to_sleep without a `zz`: the night ends here,
        # at once, so the next message starts a new conversation. In a dream
        # this wakes the companion.
        if asked and not consolidate:
            try:
                consolidate = companion.close()
            except Exception:
                logger.exception("clanka: ending the night failed")
        if turns is None and not consolidate:
            return
        name = "clanka-sleep" if consolidate else "clanka-reflect"
        self._off_thread(self._after_turn, turns, consolidate, name=name)

    def _off_thread(self, target, *args, name):
        # A copied context keeps Hermes's live model selection for ctx.llm.
        run = contextvars.copy_context().run
        threading.Thread(target=run, args=(target, *args), name=name).start()

    def on_session_reset(self, reason="", **_):
        # Off the hook thread: Hermes gives a hook callback 30s, and sleep is
        # a model call plus a pass over memory. The work lock keeps it after
        # the reflection on the last exchange.
        logger.info("clanka: session reset (%s): sleeping", reason or "unknown")
        self._off_thread(self._reset, name="clanka-sleep")

    def on_session_finalize(self, session_id=None, platform="", reason="", **_):
        # The conversation is over without a /new: quitting the CLI, closing
        # the TUI, a gateway viewer detaching. Without this the session stays
        # open and the next conversation inherits it. /new fires this and then
        # on_session_reset; the second finds nothing said and does nothing.
        #
        # One gateway serves every chat of the profile and the companion has
        # one conversation, so only the end of the chat it is in counts, and
        # only an end someone chose (DELIBERATE_ENDS). Anything else leaves
        # the conversation open, as before: the next goodnight, /new or
        # `clanka doctor end-session` ends it. A process that never had a turn
        # has no chat here, so a bare quit never opens the companion.
        deliberate = reason in DELIBERATE_ENDS or (reason == "shutdown" and platform == "cli")
        # A scripted run (hermes/bench.sh) talks through one one-shot per
        # message, so leaving must not end its conversation; /new still does.
        if self._ctx.get_config("sleep_on_leave", True) in (False, "false") and reason != "new_session":
            deliberate = False
        if session_id is None or session_id != self._live_chat or not deliberate:
            logger.info(
                "clanka: chat %s ended (%s, %s): not the conversation's end; awake",
                session_id, platform or "unknown", reason or "no reason",
            )
            return
        self._live_chat = None
        self.on_session_reset(reason=f"finalize: {reason}")

    def _reset(self):
        # A dream the conversation ended before the companion woke from is
        # dropped: nothing of it is kept. A night with nothing said since the
        # companion last slept is no night; the host's boundary costs nothing.
        try:
            c = self.companion()
            if c.drop_dream():
                logger.info("clanka: the conversation ended mid-dream: the dream is dropped")
                return
            closed = c.close()
        except Exception:
            logger.exception("clanka: ending the conversation failed")
            return
        if closed:
            self._after_turn(None, True)

    def _after_turn(self, turns, consolidate):
        with self._work_lock:
            c = self.companion()
            # When the night ended, sleep sees the whole conversation, this
            # exchange included, and writes the night from it: a separate
            # reflection on the goodbye would only retell it. A dream's turns
            # are never reflected on.
            if turns is not None and not consolidate:
                try:
                    ok, refused, left_out, caught_up = c.reflect(*turns)
                    if caught_up:
                        logger.info(
                            "clanka: caught up on %d earlier exchange(s) before %s",
                            caught_up, turns,
                        )
                    # What the model got wrong, so the protocol can be fixed where it fails.
                    if refused:
                        logger.warning(
                            "clanka: reflected on %s: %d ok, %d refused: %s",
                            turns, ok, len(refused), " | ".join(refused),
                        )
                    else:
                        logger.info("clanka: reflected on %s: %d ok", turns, ok)
                    if left_out:
                        logger.warning("clanka: facts left out: %s", " | ".join(left_out))
                except Exception:
                    logger.exception("clanka: reflection on %s failed", turns)
            if consolidate:
                # Every night and dream that ended and waits, this one last.
                try:
                    for line in c.consolidate():
                        logger.info("clanka: %s", line)
                        self._post(line)
                except Exception:
                    logger.exception("clanka: sleep failed")
                    self._post("Sleep failed; it is tried again after the next night.")
                # The conversation is over: what clanka noted of it goes to the
                # endpoint its owner named, if they named one. A send that
                # fails costs nothing; its events go with the next.
                try:
                    from . import _clanka

                    sent = _clanka.send_diagnostics()
                    if sent:
                        logger.info("clanka: sent %d diagnostics events", sent)
                except Exception as e:
                    logger.warning("clanka: diagnostics were not sent: %s", e)

    def _post(self, line):
        # Into the chat this turn or reset is for, through its adapter: Hermes
        # keeps it out of the transcript, so the reply model never reads it.
        # The line holds counts and fixed words only, never anything said or
        # remembered: it shows in group chats, and a platform keeps it.
        # Without post_message (an older Hermes), or with no chat to post to,
        # the line stays in the log.
        post = getattr(self._ctx, "post_message", None)
        if post is None:
            return
        try:
            posted = post(NOTICE + line)
            if not getattr(posted, "ok", False):
                logger.info("clanka: report not posted: %s", getattr(posted, "error", None))
        except Exception:
            logger.exception("clanka: posting the report failed")

    def triage_command(self, raw_args="", **_):
        # Hermes shows what this returns as the command's answer: no turn is
        # admitted and the model never reads it. It is clanka's own report,
        # counts and fixed words only, so it is safe in a group chat and in a
        # bug report.
        try:
            companion = self.companion(TRIAGE_TIMEOUT)
        except concurrent.futures.TimeoutError:
            return NOTICE + "memory has not opened yet, so there is nothing to show."
        except Exception as e:
            logger.exception("clanka: memory did not open for /triage")
            return NOTICE + _no_triage(e)
        try:
            return companion.triage()
        except Exception as e:
            logger.exception("clanka: /triage failed")
            return NOTICE + _no_triage(e)

    def lore_command(self, raw_args="", **_):
        # Like /triage, never a turn: the user reads the lore, the model doesn't.
        try:
            return self.companion(TRIAGE_TIMEOUT).lore(raw_args or "")
        except concurrent.futures.TimeoutError:
            return NOTICE + "memory has not opened yet; try again in a moment."
        except Exception as e:
            logger.exception("clanka: /lore failed")
            return NOTICE + f"lore: {e}"

    def read_lore(self, args, **_):
        try:
            return json.dumps({"ok": True, "entry": self.companion().read_lore(str(args.get("entry", "")))})
        except Exception as e:
            return json.dumps({"ok": False, "note": str(e)})

    def recall_memory(self, args, **_):
        found = self.companion().recall(str(args.get("query", "")))
        return json.dumps({"memories": found or [], "note": "" if found else "Nothing comes to mind."})

    def wake_up(self, args, **_):
        # Only what the dream told the companion wakes it, never what it made
        # up; when it wakes, end_turn says so and the reply is the moment.
        wakes, note = self.companion().wake_up(str(args.get("because", "")))
        return json.dumps({"ok": wakes, "note": note})

    def sleep_command(self, raw_args="", session_key="", **_):
        # A slash command never reaches the model, so the goodnight goes in as
        # the user's next message, marked: the turn then runs like a typed `zz`.
        # Without a key Hermes picks the session whose command this is.
        message = f"zz {raw_args.strip()}".strip()
        if self._ctx.inject_message(message, session_key=session_key or None):
            return None
        self._note("goodnight_not_sent")
        return (
            "Couldn't say goodnight for you here. Send zz, with your goodnight after it if "
            "you like, and the companion falls asleep after its reply."
        )

    def go_to_sleep(self, args, **_):
        self._sleep_requested = True
        # Nothing a model could narrate back as its goodnight ("Sleeping now."):
        # the result names no action, only whose words the reply is.
        return json.dumps({"ok": True, "note": "Your reply is the last of this conversation, in your own words."})


def register(ctx):
    # Load the native core now, so a broken build fails at plugin load and the
    # process that holds it is known before the first turn. The companion
    # itself still opens lazily, on the first hook.
    from . import _clanka

    plugin = _Plugin(ctx)
    ctx.register_hook("pre_llm_call", plugin.pre_llm_call)
    ctx.register_hook("transform_llm_output", _dashless)
    ctx.register_hook("post_llm_call", plugin.post_llm_call)
    ctx.register_hook("api_request_error", plugin.api_request_error)
    ctx.register_hook("on_session_reset", plugin.on_session_reset)
    ctx.register_hook("on_session_finalize", plugin.on_session_finalize)
    ctx.register_tool(
        name="recall_memory",
        toolset="clanka",
        schema={
            "name": "recall_memory",
            "description": "Optional. Search your long-term memory when you deliberately try to "
            "remember something. What is relevant to this turn is already in your context.",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": "What you are trying to remember, in a few words."}},
                "required": ["query"],
            },
        },
        handler=plugin.recall_memory,
    )
    ctx.register_tool(
        name="go_to_sleep",
        toolset="clanka",
        schema={
            "name": "go_to_sleep",
            "description": "Call this before you reply whenever the user says goodnight in any "
            "spelling or language (goodnight, good night, gn, night night, nighty night, sleep "
            "well), says they are going to bed or done for the night, or asks you to sleep. You "
            "fall asleep after this reply and the conversation becomes a memory.",
            "parameters": {"type": "object", "properties": {}},
        },
        handler=plugin.go_to_sleep,
    )
    ctx.register_command(
        "sleep",
        plugin.sleep_command,
        description="Say goodnight: the companion answers, then falls asleep (in a dream, it wakes)",
        args_hint="[your goodnight]",
    )
    ctx.register_command(
        "triage",
        plugin.triage_command,
        description="What clanka did lately: the companion's state and last events, no chat text",
    )
    ctx.register_command(
        "lore",
        plugin.lore_command,
        description="The world's lore: list entries, read one, or strike a companion's line",
        args_hint="[<id> | strike <id> <line>]",
    )
    description, parameters = _clanka.read_lore_tool()
    ctx.register_tool(
        name="read_lore",
        toolset="clanka",
        schema={"name": "read_lore", "description": description, "parameters": json.loads(parameters)},
        handler=plugin.read_lore,
    )
    # The core's own wake_up, so every front end tells the model the same.
    description, parameters = _clanka.wake_up_tool()
    ctx.register_tool(
        name="wake_up",
        toolset="clanka",
        schema={"name": "wake_up", "description": description, "parameters": json.loads(parameters)},
        handler=plugin.wake_up,
    )
