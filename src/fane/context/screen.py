"""Screen capture with several backends, tried in order of quietness."""

from __future__ import annotations

import asyncio
import logging
import os
import shutil
import time
import uuid
from pathlib import Path
from urllib.parse import unquote, urlparse

from dbus_next import Message, MessageType, Variant
from PIL import Image, ImageChops, ImageStat

from fane.config import RUNTIME_DIR
from fane.context.shell import ShellHelper

log = logging.getLogger(__name__)


class ScreenCapture:
    def __init__(self, helper: ShellHelper):
        self.helper = helper
        self.backend: str | None = None
        self._tmp = RUNTIME_DIR / "fane"  # the helper extension only writes here
        self._tmp.mkdir(mode=0o700, parents=True, exist_ok=True)
        self._portal_failed = False

    async def grab(self) -> Image.Image | None:
        for name, fn in self._backends():
            try:
                img = await fn()
            except Exception as e:
                log.debug("screenshot backend %s failed: %s", name, e)
                img = None
            if img is not None:
                if self.backend != name:
                    log.info("screenshot backend: %s", name)
                    self.backend = name
                return img
        return None

    def _backends(self):
        if self.helper.available:
            yield "gnome-helper", self._helper
        if os.environ.get("XDG_SESSION_TYPE") == "x11":
            yield "x11", self._x11
        if shutil.which("grim") and os.environ.get("XDG_CURRENT_DESKTOP", "").lower() != "gnome":
            yield "grim", self._grim
        if not self._portal_failed and self.helper.bus is not None:
            yield "portal", self._portal

    async def _helper(self) -> Image.Image | None:
        path = self._tmp / "shot.png"
        if await self.helper.screenshot(str(path)):
            return _load(path)
        return None

    async def _x11(self) -> Image.Image | None:
        from PIL import ImageGrab

        return await asyncio.to_thread(ImageGrab.grab)

    async def _grim(self) -> Image.Image | None:
        path = self._tmp / "shot.png"
        proc = await asyncio.create_subprocess_exec("grim", str(path))
        await proc.wait()
        return _load(path) if proc.returncode == 0 else None

    async def _portal(self) -> Image.Image | None:
        """xdg-desktop-portal Screenshot (may ask for permission the first time)."""
        bus = self.helper.bus
        token = "fane" + uuid.uuid4().hex[:12]
        sender = bus.unique_name.lstrip(":").replace(".", "_")
        handle = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
        loop = asyncio.get_running_loop()
        fut: asyncio.Future = loop.create_future()

        def on_msg(msg: Message):
            if msg.message_type == MessageType.SIGNAL and msg.path == handle and msg.member == "Response":
                if not fut.done():
                    fut.set_result(msg.body)

        rule = f"type='signal',interface='org.freedesktop.portal.Request',path='{handle}'"
        await bus.call(Message(destination="org.freedesktop.DBus", path="/org/freedesktop/DBus",
                               interface="org.freedesktop.DBus", member="AddMatch", signature="s", body=[rule]))
        bus.add_message_handler(on_msg)
        try:
            reply = await bus.call(Message(
                destination="org.freedesktop.portal.Desktop",
                path="/org/freedesktop/portal/desktop",
                interface="org.freedesktop.portal.Screenshot",
                member="Screenshot",
                signature="sa{sv}",
                body=["", {"handle_token": Variant("s", token), "interactive": Variant("b", False)}],
            ))
            if reply.message_type == MessageType.ERROR:
                self._portal_failed = True
                return None
            code, results = await asyncio.wait_for(fut, 30)
        finally:
            bus.remove_message_handler(on_msg)
        if code != 0:
            log.warning("portal screenshot denied (code %s)", code)
            self._portal_failed = True
            return None
        path = Path(unquote(urlparse(results["uri"].value).path))
        img = _load(path)
        # The portal saves a file for every request; remove the one it just made for us.
        try:
            if time.time() - path.stat().st_mtime < 15:
                path.unlink()
        except OSError:
            pass
        return img


def _load(path: Path) -> Image.Image:
    with Image.open(path) as im:
        im.load()
        return im.copy()


def change_ratio(a: Image.Image | None, b: Image.Image | None) -> float:
    """Fraction (0..1) of a coarse grid that visibly changed between two screenshots."""
    if a is None or b is None or a.size != b.size:
        return 1.0
    size = (160, 90)
    da = a.convert("L").resize(size)
    db = b.convert("L").resize(size)
    changed = ImageChops.difference(da, db).point(lambda v: 255 if v > 20 else 0)
    return ImageStat.Stat(changed).mean[0] / 255.0
