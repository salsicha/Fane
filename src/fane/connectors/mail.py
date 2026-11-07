"""Read recent mail from local Thunderbird profiles (mbox stores), read-only."""

from __future__ import annotations

import asyncio
import email
import email.policy
import re
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Any

from fane.config import expand

TAIL_BYTES = 4 * 1024 * 1024
INBOX_NAMES = {"inbox", "INBOX", "Inbox"}


class ThunderbirdMail:
    def __init__(self, profile_roots: list[str]):
        self.roots = [p for p in (expand(r) for r in profile_roots) if p.is_dir()]

    def mailboxes(self) -> list[Path]:
        boxes = []
        for root in self.roots:
            for store in ("Mail", "ImapMail"):
                for p in root.glob(f"*/{store}/*/*"):
                    if p.is_file() and p.name in INBOX_NAMES:
                        boxes.append(p)
        return boxes

    def _read_tail(self, box: Path) -> list[dict[str, Any]]:
        size = box.stat().st_size
        with box.open("rb") as f:
            f.seek(max(0, size - TAIL_BYTES))
            data = f.read()
        chunks = re.split(rb"(?:^|\n)From [^\n]*\n", data)
        if size > TAIL_BYTES:
            chunks = chunks[1:]  # first chunk is probably a partial message
        out = []
        for raw in chunks:
            if not raw.strip():
                continue
            msg = email.message_from_bytes(raw, policy=email.policy.default)
            if not msg["subject"] and not msg["from"]:
                continue
            body = msg.get_body(preferencelist=("plain", "html"))
            text = ""
            if body is not None:
                try:
                    text = body.get_content()
                except Exception:
                    text = ""
            text = re.sub(r"<[^>]+>", " ", text)
            text = re.sub(r"\s+", " ", text).strip()
            try:
                date = parsedate_to_datetime(msg["date"]).isoformat() if msg["date"] else ""
            except (TypeError, ValueError):
                date = msg["date"] or ""
            out.append({"from": str(msg["from"] or ""), "to": str(msg["to"] or ""),
                        "subject": str(msg["subject"] or ""), "date": date,
                        "account": box.parent.name, "body": text[:4000]})
        return out

    def _recent(self, limit: int, query: str) -> list[dict[str, Any]]:
        msgs = []
        for box in self.mailboxes():
            try:
                msgs.extend(self._read_tail(box))
            except OSError:
                continue
        if query:
            q = query.lower()
            msgs = [m for m in msgs if q in (m["subject"] + m["from"] + m["body"]).lower()]
        msgs.sort(key=lambda m: m["date"], reverse=True)
        return msgs[:limit]

    async def recent(self, limit: int = 10, query: str = "", include_body: bool = False) -> Any:
        if not self.roots:
            return "No Thunderbird profile found. Connect your mail through an MCP server instead."
        msgs = await asyncio.to_thread(self._recent, int(limit), query)
        for m in msgs:
            m["body"] = m["body"] if include_body else m["body"][:240]
        return msgs or "No messages found."
