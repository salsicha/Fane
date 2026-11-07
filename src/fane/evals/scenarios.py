"""Eval cases: what a good assistant should do on each synthetic screen.

Checks are deliberately simple keyword/trace tests so results are reproducible
and cheap; read the saved transcripts for anything subtler.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any, Callable

from fane.agent import AgentResult
from fane.connectors import Tool, ToolRegistry, params
from fane.connectors.builtin import builtin_tools

# --- proactive suggestions ----------------------------------------------------------


@dataclass
class SuggestCase:
    scene: str
    # Screen understanding: the `activity` sentence should mention one of these.
    activity_any: list[str]
    # "suggest": a confident suggestion matching `suggest_any` is expected.
    # "quiet":   nothing confident should be suggested.
    # None:      suggestion behaviour isn't scored (either is reasonable).
    expect: str | None
    suggest_any: list[str] = field(default_factory=list)


SUGGEST_CASES = [
    SuggestCase("email", ["dana", "design review", "email", "reschedul"], "suggest",
                ["reply", "respond", "dana"]),
    SuggestCase("code", ["keyerror", "test", "error", "pytest", "debug"], "suggest",
                ["keyerror", "user_id", "fix", "test", "error"]),
    SuggestCase("chat", ["priya", "q3", "slack", "summary"], "suggest",
                ["q3", "summary", "summar", "priya"]),
    SuggestCase("spreadsheet", ["spreadsheet", "budget", "sheet", "div"], "suggest",
                ["div", "error", "formula", "podcast", "zero"]),
    SuggestCase("idle", ["desktop", "idle", "nothing", "no app", "wallpaper", "home screen"], "quiet"),
    SuggestCase("photos", ["photo", "picture", "image", "files", "browsing"], "quiet"),
    SuggestCase("article", ["aurora", "wikipedia", "reading", "article"], None),
]

# --- agent tasks ----------------------------------------------------------------------

Q3_NOTE = ("# Q3 plan\n- Launch the mobile beta in August\n- Hire two engineers (backend + iOS)\n"
           "- Marketing budget: $40k, mostly podcasts\n- Retire the legacy billing service")
DANA_EMAIL = {"from": "Dana Kim <dana@example.com>", "subject": "Can we move the design review?",
              "date": "2026-11-05T10:02:00", "body": "Something came up this afternoon. Could we push "
              "today's design review (2:30pm) to later this week? Tuesday or Thursday afternoon both work."}
NEWSLETTER = {"from": "Weekly Digest <news@example.com>", "subject": "This week in tech",
              "date": "2026-11-04T08:00:00", "body": "Top stories..."}
CLIPBOARD = "hey, cant make it tmrw, can we do fri instead? thx"
CALENDAR = "10:00-10:15 Standup; 14:30-15:30 Design review with Dana Kim (room 4B)"


class Mocks:
    """Fake connectors that record what the model did."""

    def __init__(self) -> None:
        self.inserted: list[str] = []
        self.delegated: list[dict[str, Any]] = []

    def registry(self) -> ToolRegistry:
        async def insert_text(text: str = "") -> str:
            self.inserted.append(text)
            return "inserted (eval)"

        async def insert_image(path: str = "") -> str:
            return "error: no such file"

        async def read_clipboard() -> str:
            return CLIPBOARD

        async def list_windows() -> list[dict[str, Any]]:
            return [{"app": "firefox", "title": "Inbox - Gmail"}, {"app": "slack", "title": "#planning"},
                    {"app": "code", "title": "handlers.py — api"}]

        async def search_notes(query: str = "", limit: int = 8) -> Any:
            if re.search(r"q3|quarter|plan|roadmap", query, re.I):
                # Only the first item: a good answer has to open the note.
                return [{"path": "/home/alex/Notes/q3-plan.md", "title": "q3-plan",
                         "snippet": "# Q3 plan - Launch the mobile beta in August …"}]
            return []

        async def read_note(path: str = "") -> str:
            return Q3_NOTE if "q3" in path.lower() else "error: no such note"

        async def recent_emails(limit: int = 10, query: str = "", include_body: bool = False) -> Any:
            msgs = [DANA_EMAIL, NEWSLETTER]
            if query:
                q = query.lower()
                hits = [m for m in msgs if any(w in (m["from"] + m["subject"] + m["body"]).lower()
                                               for w in q.split())]
                msgs = hits or msgs
            return msgs[: int(limit or 10)]

        async def delegate(agent: str = "", task: str = "", cwd: str = "") -> str:
            self.delegated.append({"agent": agent, "task": task, "cwd": cwd})
            return f"{agent or 'claude'}: replaced request.json['user_id'] with session['uid']; 12 tests pass."

        async def todays_events() -> str:
            return CALENDAR

        handlers = {"insert_text": insert_text, "insert_image": insert_image,
                    "read_clipboard": read_clipboard, "list_windows": list_windows,
                    "search_notes": search_notes, "read_note": read_note,
                    "recent_emails": recent_emails, "delegate": delegate}
        reg = ToolRegistry()
        for tool in builtin_tools(handlers, ["claude", "codex"]):
            reg.add(tool)
        # Stands in for a tool coming from a calendar MCP server.
        reg.add(Tool("calendar__todays_events", "[calendar] List today's calendar events.",
                     params(), todays_events, "Asking calendar"))
        return reg


Check = Callable[[AgentResult, Mocks], bool]


def _text(r: AgentResult, m: Mocks) -> str:
    return (r.text + " " + " ".join(m.inserted)).lower()


def called(name: str) -> Check:
    return lambda r, m: name in r.steps


def answer_has(*words: str) -> Check:
    return lambda r, m: any(w in r.text.lower() for w in words)


def inserted_has(*words: str) -> Check:
    return lambda r, m: any(w in t.lower() for t in m.inserted for w in words)


def no_preamble(r: AgentResult, m: Mocks) -> bool:
    return bool(m.inserted) and not re.match(
        r"\s*(here('s| is)|sure|certainly|of course|okay)", m.inserted[-1], re.I)


def facts(n: int, *groups: tuple[str, ...]) -> Check:
    """At least n of the facts are mentioned; each fact is a tuple of alternative phrasings."""
    return lambda r, m: sum(any(w in _text(r, m) for w in g) for g in groups) >= n


@dataclass
class AgentTask:
    name: str
    scene: str
    prompt: str
    checks: dict[str, Check]


AGENT_TASKS = [
    AgentTask("reply_insert", "email", "Reply to Dana saying Thursday at 3pm works for me.", {
        "calls insert_text": called("insert_text"),
        "mentions Thursday": inserted_has("thursday"),
        "no preamble in inserted text": no_preamble,
    }),
    AgentTask("email_lookup", "idle", "What did Dana email me about?", {
        "calls recent_emails": called("recent_emails"),
        "answer: design review / reschedule": answer_has("design review", "reschedul", "move", "push"),
    }),
    AgentTask("notes_summary", "chat", "Summarize my notes on the Q3 plan so I can send it to Priya.", {
        "calls search_notes": called("search_notes"),
        "opens the note": called("read_note"),
        "≥3 of 4 facts from the note": facts(3, ("mobile beta", "august"),
                                             ("two engineers", "2 engineers", "hire"),
                                             ("40k", "40,000", "$40"), ("billing",)),
    }),
    AgentTask("screen_read", "code", "What's the error on my screen and on which line?", {
        "names KeyError": answer_has("keyerror"),
        "names line 42": answer_has("42"),
        "answers from the screen (≤1 tool, no delegation/insert)": lambda r, m: (
            len(r.calls) <= 1 and not m.delegated and not m.inserted),
    }),
    AgentTask("delegate_fix", "code",
              "This needs a proper fix across the codebase. Get the failing test passing in ~/code/api.", {
        "calls delegate": called("delegate"),
        "task describes the failure": lambda r, m: any(
            re.search(r"user_id|keyerror|test_get_profile|handlers", d["task"], re.I) for d in m.delegated),
    }),
    AgentTask("clipboard_rewrite", "chat",
              "Rewrite what's on my clipboard to sound professional and put it where I'm typing.", {
        "calls read_clipboard": called("read_clipboard"),
        "calls insert_text": called("insert_text"),
        "keeps the meaning (Friday)": inserted_has("friday", "fri"),
        "drops the slang": lambda r, m: bool(m.inserted) and "thx" not in m.inserted[-1].lower(),
    }),
    AgentTask("calendar_mcp", "email", "Am I free at 2:30 this afternoon?", {
        "calls the calendar tool": called("calendar__todays_events"),
        "answer: busy (design review)": lambda r, m: bool(re.search(
            r"\bno\b|not free|busy|design review|conflict|booked", r.text, re.I)),
        "doesn't insert or delegate": lambda r, m: not m.inserted and not m.delegated,
    }),
]
