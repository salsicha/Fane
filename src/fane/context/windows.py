"""Information about the focused window and running apps."""

from __future__ import annotations

import asyncio
import os
import re
import shutil
from dataclasses import dataclass, field
from typing import Any

from fane.context.shell import ShellHelper


@dataclass
class WindowInfo:
    title: str = ""
    app: str = ""  # wm_class or app id
    pid: int = 0
    extra: dict[str, Any] = field(default_factory=dict)

    @property
    def label(self) -> str:
        if self.app and self.title:
            return f"{self.app} — {self.title}"
        return self.title or self.app or "unknown window"

    @property
    def is_terminal(self) -> bool:
        terms = ("terminal", "kitty", "alacritty", "ptyxis", "konsole", "wezterm",
                 "foot", "tilix", "terminator", "xterm", "ghostty", "console")
        return any(t in self.app.lower() for t in terms)


async def _run(*cmd: str) -> str:
    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
    out, _ = await proc.communicate()
    return out.decode(errors="replace")


async def focused_window(helper: ShellHelper) -> WindowInfo:
    if helper.available and (w := await helper.focused_window()):
        return WindowInfo(title=w.get("title", ""), app=w.get("app", ""), pid=w.get("pid", 0),
                          extra={k: v for k, v in w.items() if k not in ("title", "app", "pid")})
    if os.environ.get("DISPLAY") and shutil.which("xprop"):
        # Only sees X11/XWayland windows, but better than nothing.
        root = await _run("xprop", "-root", "_NET_ACTIVE_WINDOW")
        m = re.search(r"(0x[0-9a-f]+)", root)
        if m and m.group(1) != "0x0":
            props = await _run("xprop", "-id", m.group(1), "_NET_WM_NAME", "WM_CLASS", "_NET_WM_PID")
            title = re.search(r'_NET_WM_NAME\(\w+\) = "(.*)"', props)
            cls = re.findall(r'"([^"]*)"', props.split("WM_CLASS", 1)[-1].split("\n", 1)[0])
            pid = re.search(r"_NET_WM_PID\(\w+\) = (\d+)", props)
            return WindowInfo(title=title.group(1) if title else "", app=cls[-1] if cls else "",
                              pid=int(pid.group(1)) if pid else 0)
    return WindowInfo()


async def list_windows(helper: ShellHelper) -> list[dict[str, Any]]:
    if helper.available:
        return await helper.list_windows()
    if shutil.which("wmctrl"):
        out = await _run("wmctrl", "-lpx")
        wins = []
        for line in out.splitlines():
            parts = line.split(None, 4)
            if len(parts) == 5:
                wins.append({"app": parts[3], "pid": int(parts[2]), "title": parts[4].split(" ", 1)[-1]})
        return wins
    return []
