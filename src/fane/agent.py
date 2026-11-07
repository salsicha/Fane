"""Tool-using agent loop on top of the local model."""

from __future__ import annotations

import datetime as dt
import json
import logging
from dataclasses import dataclass, field
from typing import Any, Callable

from fane.connectors import ToolRegistry
from fane.llm import Ollama, encode_image
from fane.snapshot import Snapshot

log = logging.getLogger(__name__)

MAX_STEPS = 8

SYSTEM = """\
You are Fane, a quiet desktop assistant running locally on the user's Ubuntu computer.
You can see the user's screen and you have tools to read their notes and email, list
their open windows, read the clipboard, insert text or images at the user's text cursor,
and delegate hard or long tasks (especially coding) to Claude Code or Codex.

Rules:
- When the user wants text written, filled in, replied or rewritten where they are typing,
  compose it and call insert_text exactly once with only the final text (no preamble).
- Use tools to look things up instead of guessing. Never invent emails or notes.
- Delegate programming, multi-file or long research tasks to the `delegate` tool.
- Keep replies short and plain. Answer directly; no filler.
"""


@dataclass
class AgentResult:
    text: str
    inserted: bool = False
    steps: list[str] = field(default_factory=list)


class Agent:
    def __init__(self, llm: Ollama, tools: ToolRegistry, cfg: dict[str, Any]):
        self.llm = llm
        self.tools = tools
        self.cfg = cfg

    def _context_text(self, snap: Snapshot, history: list[str]) -> str:
        lines = [f"Time: {dt.datetime.now():%A %Y-%m-%d %H:%M}",
                 f"Focused window: {snap.window.label}"]
        if history:
            lines.append("Recent activity: " + " | ".join(history[-5:]))
        if snap.image is None:
            lines.append("(No screenshot available.)")
        return "\n".join(lines)

    async def run(self, prompt: str, snap: Snapshot, history: list[str],
                  on_status: Callable[[str], None]) -> AgentResult:
        user: dict[str, Any] = {"role": "user",
                                "content": f"{self._context_text(snap, history)}\n\nRequest: {prompt}"}
        if snap.image is not None:
            user["images"] = [encode_image(snap.image, self.cfg["max_image_side"])]
        messages: list[dict[str, Any]] = [{"role": "system", "content": SYSTEM}, user]
        result = AgentResult(text="")

        for _ in range(MAX_STEPS):
            on_status("Thinking…")
            msg = await self.llm.chat(messages, tools=self.tools.specs())
            messages.append({k: v for k, v in msg.items() if k in ("role", "content", "tool_calls")})
            calls = msg.get("tool_calls") or []
            if not calls:
                result.text = (msg.get("content") or "").strip()
                return result
            for call in calls:
                fn = call.get("function", {})
                name = fn.get("name", "")
                args = fn.get("arguments") or {}
                if isinstance(args, str):
                    try:
                        args = json.loads(args)
                    except json.JSONDecodeError:
                        args = {}
                tool = self.tools.tools.get(name)
                on_status((tool.status if tool and tool.status else f"Using {name}") + "…")
                log.info("tool %s(%s)", name, json.dumps(args)[:200])
                output = await self.tools.call(name, args)
                result.steps.append(name)
                if name in ("insert_text", "insert_image") and output.startswith("inserted"):
                    result.inserted = True
                messages.append({"role": "tool", "tool_name": name, "content": output})
            if result.inserted:
                # The deliverable is already in the user's document; one more turn for a short note.
                messages.append({"role": "user", "content": "The text is now at the user's cursor "
                                 "(nothing was sent). Reply with one short line, no tools."})
                msg = await self.llm.chat(messages)
                result.text = (msg.get("content") or "Inserted.").strip()
                return result
        result.text = result.text or "Stopped after too many steps."
        return result
