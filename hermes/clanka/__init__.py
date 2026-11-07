"""clanka for Hermes Agent: long-term memory for one companion.

A general plugin on Hermes's public plugin API only:

- ``pre_llm_call``: checks for the doctor token (the turn is blocked and memory
  frozen), then records the user's message and adds the turn's memory, mood
  and notes as context.
- ``post_llm_call``: records the reply, then reflects on the exchange on a
  background thread. Reflection runs on ``ctx.llm``, the user's active model,
  so the companion's memory is written by the same model it chats with.
- Tools ``recall_memory`` and ``go_to_sleep``. Sleep waits until the reply is
  recorded, then consolidates the conversation.

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
        result = self._ctx.llm.complete(
            [{"role": "system", "content": system}, *messages],
            temperature=temperature,
            max_tokens=MAX_TOKENS,
            purpose="clanka.memory",
        )
        return result.text

    def pre_llm_call(self, user_message="", **_):
        kind, context = self.companion().begin_turn(user_message or "")
        if kind == "doctor":
            return {"action": "block", "message": "Memory frozen."}
        if kind == "open":
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

    def _after_turn(self, turns, sleep):
        with self._work_lock:
            c = self.companion()
            if turns is not None:
                try:
                    ok, refused = c.reflect(*turns)
                    logger.info("clanka: reflected on %s: %d ok, %d refused", turns, ok, refused)
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
        return json.dumps({"ok": True, "note": "You fall asleep right after this reply. Say goodnight."})


def register(ctx):
    plugin = _Plugin(ctx)
    ctx.register_hook("pre_llm_call", plugin.pre_llm_call)
    ctx.register_hook("post_llm_call", plugin.post_llm_call)
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
