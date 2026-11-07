"""Plain-text / Markdown notes folders (Obsidian vaults, ~/Notes, ...)."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from fane.config import expand

NOTE_SUFFIXES = {".md", ".markdown", ".txt", ".org"}


class Notes:
    def __init__(self, dirs: list[str]):
        self.dirs = [p for p in (expand(d) for d in dirs) if p.is_dir()]

    def _files(self):
        for root in self.dirs:
            for p in root.rglob("*"):
                if p.suffix.lower() in NOTE_SUFFIXES and p.is_file() and not any(
                    part.startswith(".") for part in p.relative_to(root).parts
                ):
                    yield p

    def _search(self, query: str, limit: int) -> list[dict[str, Any]]:
        terms = [t.lower() for t in query.split() if t]
        hits = []
        for p in self._files():
            try:
                text = p.read_text(errors="replace")
            except OSError:
                continue
            low = text.lower()
            name = p.stem.lower()
            score = sum(low.count(t) + 5 * (t in name) for t in terms) if terms else 1
            if score:
                idx = min((low.find(t) for t in terms if t in low), default=0)
                snippet = text[max(0, idx - 120): idx + 280].replace("\n", " ").strip()
                hits.append({"path": str(p), "title": p.stem, "score": score,
                             "modified": p.stat().st_mtime, "snippet": snippet})
        hits.sort(key=lambda h: (h["score"], h["modified"]), reverse=True)
        return hits[:limit]

    async def search(self, query: str = "", limit: int = 8) -> Any:
        if not self.dirs:
            return "No notes folders found. Set [connectors] notes_dirs in ~/.config/fane/config.toml."
        return await asyncio.to_thread(self._search, query, int(limit))

    async def read(self, path: str) -> str:
        p = Path(path).expanduser().resolve()
        if not any(p.is_relative_to(d.resolve()) for d in self.dirs):
            return "error: path is outside the configured notes folders"
        return p.read_text(errors="replace")[:20000]
