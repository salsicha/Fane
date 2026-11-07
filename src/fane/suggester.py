"""Proactive loop: watch the screen, and occasionally prompt itself with a suggestion."""

from __future__ import annotations

import difflib
import logging
import time
from dataclasses import dataclass
from typing import Any

from fane.context.screen import change_ratio
from fane.llm import Ollama, encode_image
from fane.snapshot import Snapshot

log = logging.getLogger(__name__)

SCHEMA = {
    "type": "object",
    "properties": {
        "activity": {"type": "string"},
        "suggestions": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "prompt": {"type": "string"},
                    "confidence": {"type": "number"},
                },
                "required": ["title", "prompt", "confidence"],
            },
        },
    },
    "required": ["activity", "suggestions"],
}

PROMPT = """\
You are the proactive half of Fane, a desktop assistant. Look at the user's screen.

1. In `activity`, describe in one short sentence what the user is doing right now.
2. In `suggestions`, propose AT MOST ONE thing the assistant could do immediately that would
   clearly help with that exact activity. The assistant can: read the user's notes and email,
   draft or rewrite text and insert it at the cursor, summarize what's on screen, look things
   up in open apps, and hand coding work to Claude Code or Codex.
   - `title`: 2-6 words shown on a small card, e.g. "Draft a reply to Dana".
   - `prompt`: a self-contained instruction the assistant will execute, written as the
     user would type it, mentioning concrete names/details visible on screen.
   - `confidence`: 0-1, how sure you are the user would want this right now.

Staying quiet is better than noise: return an empty list unless the help is specific and
valuable. Never suggest generic things ("take a break", "organize files", "learn more").
Focused window: {window}
Recent activity: {history}
Do not repeat these recent suggestions: {recent}
"""


@dataclass
class Suggestion:
    id: int
    title: str
    prompt: str
    confidence: float
    window: str
    created_at: float


class Suggester:
    def __init__(self, llm: Ollama, cfg: dict[str, Any], max_image_side: int):
        self.llm = llm
        self.cfg = cfg
        self.max_image_side = max_image_side
        self.history: list[str] = []
        self.recent_titles: list[str] = []
        self._last_snap: Snapshot | None = None
        self._quiet_until = 0.0
        self._dismissals = 0
        self._next_id = 1

    # --- feedback from the UI -------------------------------------------------
    def dismissed(self) -> None:
        self._dismissals += 1
        delay = min(self.cfg["cooldown_seconds"] * 2 ** (self._dismissals - 1),
                    self.cfg["max_cooldown_seconds"])
        self._quiet_until = time.time() + delay
        log.info("suggestion dismissed; quiet for %ds", delay)

    def ignored(self) -> None:
        self._quiet_until = max(self._quiet_until, time.time() + self.cfg["cooldown_seconds"])

    def accepted(self) -> None:
        self._dismissals = 0

    # --- main step ------------------------------------------------------------
    def should_look(self, snap: Snapshot, force: bool) -> bool:
        if force:
            return True
        if time.time() < self._quiet_until:
            return False
        last = self._last_snap
        if last is None or last.window.label != snap.window.label:
            return True
        return change_ratio(last.image, snap.image) >= self.cfg["min_change"]

    def _is_repeat(self, title: str) -> bool:
        t = title.lower()
        return any(difflib.SequenceMatcher(None, t, r.lower()).ratio() > 0.75
                   for r in self.recent_titles[-20:])

    async def propose(self, snap: Snapshot) -> tuple[str, list[dict[str, Any]]]:
        """One model call: returns (activity, raw suggestions), with no filtering."""
        prompt = PROMPT.format(window=snap.window.label,
                               history=" | ".join(self.history[-4:]) or "none",
                               recent="; ".join(self.recent_titles[-8:]) or "none")
        data = await self.llm.chat_json(
            [{"role": "user", "content": prompt,
              "images": [encode_image(snap.image, self.max_image_side)]}], SCHEMA)
        if not isinstance(data, dict):
            return "", []
        raw = data.get("suggestions")
        raw = raw if isinstance(raw, list) else []
        return str(data.get("activity", "")).strip(), [s for s in raw if isinstance(s, dict)]

    async def step(self, snap: Snapshot, force: bool = False) -> Suggestion | None:
        if snap.image is None or not self.should_look(snap, force):
            return None
        self._last_snap = snap
        activity, raw = await self.propose(snap)
        if activity:
            self.history = (self.history + [activity])[-12:]
        log.info("activity: %s", activity)
        min_conf = 0.0 if force else self.cfg["min_confidence"]
        for s in raw:
            title, text = str(s.get("title", "")).strip(), str(s.get("prompt", "")).strip()
            try:
                conf = float(s.get("confidence", 0) or 0)
            except (TypeError, ValueError):
                conf = 0.0
            if not title or not text or conf < min_conf or (self._is_repeat(title) and not force):
                log.info("skipped suggestion %r (%.2f)", title, conf)
                continue
            self.recent_titles.append(title)
            sug = Suggestion(self._next_id, title, text, conf, snap.window.label, time.time())
            self._next_id += 1
            return sug
        return None
