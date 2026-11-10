"""clanka for Hermes Agent: long-term memory for one companion.

A general plugin on Hermes's public plugin API only:

- ``pre_llm_call``: checks for the doctor token (the turn is blocked and memory
  frozen), blocks every turn while memory is frozen, then records the user's
  message and adds the turn's memory, mood and notes as context.
- ``post_llm_call``: records the reply, then reflects on the exchange on a
  background thread. Reflection runs on ``ctx.llm``, the user's active model,
  so the companion's memory is written by the same model it chats with.
- Tools ``recall_memory`` and ``go_to_sleep``. Sleep waits until the reply is
  recorded, then consolidates the conversation.
- ``on_session_reset`` (``/new``, ``/reset``, a session-reset policy plugin):
  the companion sleeps on whatever was said since it last slept, so a
  conversation the user walked away from still becomes a memory. Nothing
  happens when nothing was said.

The companion is the soul.md named by ``plugins.entries.clanka.settings.soul``
in the profile's config.yaml (``$CLANKA_SOUL`` as a fallback). Put its persona in
Hermes's SOUL.md with ``clanka soul persona <soul.md>``; never copy the whole
file, whose frontmatter holds the doctor token.
"""

from __future__ import annotations

import contextvars
import json
import logging
import os
import threading

logger = logging.getLogger(__name__)

# Most one memory-writing reply may run to.
MAX_TOKENS = 2048


class _Plugin:
    def __init__(self, ctx):
        self._ctx = ctx
        self._companion = None
        self._open_lock = threading.Lock()
        # Reflections and sleep run one at a time, in order, off the turn thread.
        self._work_lock = threading.Lock()
        self._sleep_requested = False

    # The companion opens on first use, so a slow Secret Service or model load
    # never counts against Hermes's plugin load timeout.
    def companion(self):
        with self._open_lock:
            if self._companion is None:
                from . import _clanka

                # Profile config first: under Hermes's multiplexed gateway a
                # profile's .env doesn't reach a plugin's os.environ.
                soul = self._ctx.get_config("soul") or os.environ.get("CLANKA_SOUL")
                if not soul:
                    raise RuntimeError(
                        "set plugins.entries.clanka.settings.soul to the companion's soul.md"
                    )
                self._companion = _clanka.Companion(soul, self._model)
            return self._companion

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

    def pre_llm_call(self, user_message="", is_first_turn=True, **_):
        # Hermes replays injected context on every later turn, so after the
        # first one only what the model hasn't seen is injected, and nothing
        # at all when nothing is new.
        kind, context = self.companion().begin_turn(user_message or "", bool(is_first_turn))
        if kind == "doctor":
            return {"action": "block", "message": "Memory frozen."}
        if kind == "frozen":
            # Answering would be a reply the companion never remembers.
            return {
                "action": "block",
                "message": "Memory is frozen by the doctor. `clanka doctor unfreeze` lets us talk again.",
            }
        if kind == "open" and context:
            return {"context": context}
        return None

    def post_llm_call(self, assistant_response="", **_):
        turns = self.companion().end_turn(assistant_response or "")
        sleep, self._sleep_requested = self._sleep_requested, False
        if turns is None and not sleep:
            return
        # A copied context keeps Hermes's live model selection for ctx.llm.
        run = contextvars.copy_context().run
        threading.Thread(
            target=run, args=(self._after_turn, turns, sleep), name="clanka-reflect"
        ).start()

    def on_session_reset(self, reason="", **_):
        # Off the hook thread: Hermes gives a hook callback 30s, and sleep is
        # a model call plus a pass over memory. The work lock keeps it after
        # the reflection on the last exchange.
        logger.info("clanka: session reset (%s): sleeping", reason or "unknown")
        run = contextvars.copy_context().run
        threading.Thread(target=run, args=(self._after_turn, None, True), name="clanka-sleep").start()

    def _after_turn(self, turns, sleep):
        with self._work_lock:
            c = self.companion()
            # When the companion asked to sleep, sleep sees the whole conversation,
            # this exchange included, and writes the night from it: a separate
            # reflection on the goodbye would only retell it.
            if turns is not None and not sleep:
                try:
                    ok, refused, left_out = c.reflect(*turns)
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
            if sleep:
                try:
                    logger.info("clanka: %s", c.sleep())
                except Exception:
                    logger.exception("clanka: sleep failed")

    def recall_memory(self, args, **_):
        found = self.companion().recall(str(args.get("query", "")))
        return json.dumps({"memories": found or [], "note": "" if found else "Nothing comes to mind."})

    def go_to_sleep(self, args, **_):
        self._sleep_requested = True
        # Nothing a model could narrate back as its goodnight ("Sleeping now."):
        # the result names no action, only whose words the reply is.
        return json.dumps({"ok": True, "note": "Your reply is the goodnight, in your own words."})


def register(ctx):
    # Load the native core now, so a broken build fails at plugin load and the
    # process that holds it is known before the first turn. The companion
    # itself still opens lazily, on the first hook.
    from . import _clanka  # noqa: F401

    plugin = _Plugin(ctx)
    ctx.register_hook("pre_llm_call", plugin.pre_llm_call)
    ctx.register_hook("post_llm_call", plugin.post_llm_call)
    ctx.register_hook("on_session_reset", plugin.on_session_reset)
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
            "description": "Call when the user says goodnight or asks you to sleep. You fall "
            "asleep after this reply and the conversation becomes a memory.",
            "parameters": {"type": "object", "properties": {}},
        },
        handler=plugin.go_to_sleep,
    )
