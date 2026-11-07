"""Tool-using agent loop on top of the local model.

Two calling conventions:
- native: Ollama's `tools` API (Gemma 4, Qwen 3.8, ...)
- json:   for models without native tool calling (e.g. MiniCPM-V 4.7), the model
          answers with {"action": ..., "arguments": ...} under a JSON schema.
"""

from __future__ import annotations

import datetime as dt
import json
import logging
from dataclasses import dataclass, field
from typing import Any, Callable

from fane.connectors import ToolRegistry
from fane.llm import LLMError, Ollama, ToolsUnsupported, encode_image, parse_json
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
- When the user asks a question, just answer it. Don't insert or send anything they
  didn't ask for.
- If the screen already shows the answer, answer from the screen without tools.
- Use tools to look things up instead of guessing, and only the tools you need.
  Never invent emails or notes.
- Delegate programming, multi-file or long research tasks to the `delegate` tool.
- Keep replies short and plain. Answer directly; no filler.
"""

JSON_PROTOCOL = """
You call tools by replying with JSON only, one action per reply:
  {{"action": "<tool name>", "arguments": {{...}}}}
When you are finished, reply:
  {{"action": "final", "answer": "<your reply to the user>"}}
Tool results come back in the next message. Most requests need zero or one tool: if the
screen or a previous result already answers the request, reply with "final" right away.

Tools:
{tools}
"""

DONE_NOTE = ("The text is now at the user's cursor (nothing was sent). "
             "Reply with one short line, no tools.")


@dataclass
class ToolCall:
    name: str
    args: dict[str, Any]
    output: str


@dataclass
class AgentResult:
    text: str
    inserted: bool = False
    calls: list[ToolCall] = field(default_factory=list)
    mode: str = "native"

    @property
    def steps(self) -> list[str]:
        return [c.name for c in self.calls]


class Agent:
    def __init__(self, llm: Ollama, tools: ToolRegistry, cfg: dict[str, Any]):
        self.llm = llm
        self.tools = tools
        self.cfg = cfg
        # model name -> "native" | "json"; in "auto" mode this is learned on first use.
        self._modes: dict[str, str] = {}

    def mode(self) -> str:
        forced = self.cfg.get("tool_mode", "auto")
        return forced if forced in ("native", "json") else self._modes.get(self.llm.model, "native")

    def _context_text(self, snap: Snapshot, history: list[str]) -> str:
        lines = [f"Time: {dt.datetime.now():%A %Y-%m-%d %H:%M}",
                 f"Focused window: {snap.window.label}"]
        if history:
            lines.append("Recent activity: " + " | ".join(history[-5:]))
        if snap.image is None:
            lines.append("(No screenshot available.)")
        return "\n".join(lines)

    def _first_messages(self, prompt: str, snap: Snapshot, history: list[str], mode: str):
        system = SYSTEM
        if mode == "json":
            listing = "\n".join(
                f"- {t.name}: {t.description} Arguments: {json.dumps(t.parameters['properties'])}"
                for t in self.tools.tools.values())
            system += JSON_PROTOCOL.format(tools=listing)
        user: dict[str, Any] = {"role": "user",
                                "content": f"{self._context_text(snap, history)}\n\nRequest: {prompt}"}
        if snap.image is not None:
            user["images"] = [encode_image(snap.image, self.cfg["max_image_side"])]
        return [{"role": "system", "content": system}, user]

    async def run(self, prompt: str, snap: Snapshot, history: list[str],
                  on_status: Callable[[str], None]) -> AgentResult:
        mode = self.mode()
        try:
            return await self._run(prompt, snap, history, on_status, mode)
        except ToolsUnsupported:
            log.info("%s has no native tools; switching to JSON tool calls", self.llm.model)
            self._modes[self.llm.model] = "json"
            return await self._run(prompt, snap, history, on_status, "json")

    async def _run(self, prompt: str, snap: Snapshot, history: list[str],
                   on_status: Callable[[str], None], mode: str) -> AgentResult:
        messages = self._first_messages(prompt, snap, history, mode)
        result = AgentResult(text="", mode=mode)
        schema = {"type": "object", "properties": {
            # "final" first: some models treat the enum as a checklist and walk it in order.
            "action": {"type": "string", "enum": ["final", *self.tools.tools]},
            "arguments": {"type": "object"}, "answer": {"type": "string"}},
            "required": ["action"]}

        for _ in range(MAX_STEPS):
            on_status("Thinking…")
            if mode == "native":
                msg = await self.llm.chat(messages, tools=self.tools.specs())
                messages.append({k: v for k, v in msg.items() if k in ("role", "content", "tool_calls")})
                calls = [(c.get("function", {}).get("name", ""), c.get("function", {}).get("arguments"))
                         for c in msg.get("tool_calls") or []]
                final = (msg.get("content") or "").strip()
            else:
                msg = await self.llm.chat(messages, schema=schema)
                messages.append({"role": "assistant", "content": msg.get("content", "")})
                try:
                    data = parse_json(msg.get("content", ""))
                except LLMError:
                    data = {"action": "final", "answer": msg.get("content", "")}
                action = str(data.get("action", "final"))
                calls = [] if action == "final" else [(action, data.get("arguments"))]
                final = str(data.get("answer", "")).strip()

            if not calls:
                result.text = final
                return result

            for name, args in calls:
                if isinstance(args, str):
                    try:
                        args = json.loads(args)
                    except json.JSONDecodeError:
                        args = {}
                args = args if isinstance(args, dict) else {}
                tool = self.tools.tools.get(name)
                on_status((tool.status if tool and tool.status else f"Using {name}") + "…")
                log.info("tool %s(%s)", name, json.dumps(args)[:200])
                output = await self.tools.call(name, args)
                result.calls.append(ToolCall(name, args, output))
                if name in ("insert_text", "insert_image") and output.startswith("inserted"):
                    result.inserted = True
                if mode == "native":
                    messages.append({"role": "tool", "tool_name": name, "content": output})
                else:
                    messages.append({"role": "user", "content": f"Result of {name}:\n{output}"})

            if result.inserted:
                # The deliverable is already in the user's document; one more turn for a short note.
                note = DONE_NOTE + (' Answer with {"action": "final", "answer": ...}.' if mode == "json" else "")
                messages.append({"role": "user", "content": note})
                msg = await self.llm.chat(messages, schema=schema if mode == "json" else None)
                text = msg.get("content") or ""
                if mode == "json":
                    try:
                        text = str(parse_json(text).get("answer", ""))
                    except LLMError:
                        pass
                result.text = text.strip() or "Inserted."
                return result
        result.text = result.text or "Stopped after too many steps."
        return result
