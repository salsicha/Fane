"""Definitions of the built-in tools. Handlers are injected so evals can mock them."""

from __future__ import annotations

from typing import Any, Callable

from fane.connectors import Tool, params


def _s(type_: str, desc: str = "") -> dict[str, Any]:
    return {"type": type_, "description": desc}


def builtin_tools(h: dict[str, Callable[..., Any]], delegate_agents: list[str]) -> list[Tool]:
    """`h` maps tool name -> async handler."""
    tools = [
        Tool("insert_text", "Insert text at the user's text cursor in the focused app "
             "(as if pasted). Use for drafting, replying, filling in, rewriting.",
             params(["text"], text=_s("string", "The exact text to insert")),
             h["insert_text"], "Inserting"),
        Tool("insert_image", "Paste an image file at the user's cursor.",
             params(["path"], path=_s("string", "Absolute path to a PNG/JPEG file")),
             h["insert_image"], "Inserting image"),
        Tool("read_clipboard", "Read the current clipboard text.", params(),
             h["read_clipboard"], "Reading clipboard"),
        Tool("list_windows", "List the user's open windows/apps with titles.", params(),
             h["list_windows"], "Looking at open apps"),
        Tool("search_notes", "Search the user's notes (Markdown/text folders). "
             "Returns paths, titles and snippets.",
             params(["query"], query=_s("string", "Keywords"), limit=_s("integer")),
             h["search_notes"], "Searching notes"),
        Tool("read_note", "Read a note's full text by path (from search_notes).",
             params(["path"], path=_s("string")), h["read_note"], "Reading note"),
        Tool("recent_emails", "Recent inbox email from the local Thunderbird client, newest first.",
             params([], limit=_s("integer", "How many (default 10)"),
                    query=_s("string", "Optional filter text"),
                    include_body=_s("boolean", "Return full bodies")),
             h["recent_emails"], "Checking email"),
    ]
    if delegate_agents:
        tools.append(Tool(
            "delegate", "Hand a difficult task (coding, debugging, multi-step research) to a "
            f"stronger coding agent. Available: {', '.join(delegate_agents)}. Returns its final output.",
            params(["agent", "task"], agent={"type": "string", "enum": delegate_agents},
                   task=_s("string", "Complete, self-contained task description"),
                   cwd=_s("string", "Working directory (optional)")),
            h["delegate"], "Delegating"))
    return tools
