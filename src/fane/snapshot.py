"""A moment of user context: focused window plus (optionally) the screen."""

from __future__ import annotations

import time
from dataclasses import dataclass, field

from PIL import Image

from fane.context.windows import WindowInfo


@dataclass
class Snapshot:
    window: WindowInfo
    image: Image.Image | None = None
    taken_at: float = field(default_factory=time.time)
    # True when privacy rules stopped us from looking at the screen.
    private: bool = False


def is_private(window: WindowInfo, privacy: dict) -> bool:
    app = window.app.lower()
    title = window.title.lower()
    return (any(b.lower() in app for b in privacy["blocked_apps"])
            or any(b.lower() in title for b in privacy["blocked_titles"]))
