"""Talks to the optional `fane-helper` GNOME Shell extension over D-Bus.

On GNOME Wayland, ordinary apps can't see which window is focused, take silent
screenshots, or place their own windows. The extension (gnome-extension/) runs
inside the shell and exposes exactly those three things.
"""

from __future__ import annotations

import json
import logging
from typing import Any

from dbus_next import BusType
from dbus_next.aio import MessageBus
from dbus_next.errors import DBusError

log = logging.getLogger(__name__)

BUS_NAME = "io.github.salsicha.Fane"
OBJ_PATH = "/io/github/salsicha/Fane"
IFACE = "io.github.salsicha.Fane"


class ShellHelper:
    def __init__(self) -> None:
        self._bus: MessageBus | None = None
        self._iface = None
        self.available = False

    async def connect(self) -> bool:
        try:
            self._bus = await MessageBus(bus_type=BusType.SESSION).connect()
            intro = await self._bus.introspect(BUS_NAME, OBJ_PATH)
            obj = self._bus.get_proxy_object(BUS_NAME, OBJ_PATH, intro)
            self._iface = obj.get_interface(IFACE)
            self.available = True
        except Exception as e:  # not installed / not enabled / not on GNOME
            log.info("GNOME helper extension unavailable (%s)", e)
            self.available = False
        return self.available

    @property
    def bus(self) -> MessageBus | None:
        return self._bus

    async def _call(self, method: str, *args: Any) -> Any:
        if not self.available:
            return None
        try:
            return await getattr(self._iface, f"call_{method}")(*args)
        except DBusError as e:
            log.warning("helper %s failed: %s", method, e)
            return None

    async def focused_window(self) -> dict[str, Any] | None:
        raw = await self._call("focused_window")
        return json.loads(raw) if raw else None

    async def list_windows(self) -> list[dict[str, Any]]:
        raw = await self._call("list_windows")
        return json.loads(raw) if raw else []

    async def screenshot(self, path: str) -> bool:
        return bool(await self._call("screenshot", path))

    async def place_window(self, title: str, x: int, y: int, above: bool) -> bool:
        return bool(await self._call("place_window", title, x, y, above))

    async def activate_window(self, title: str) -> bool:
        return bool(await self._call("activate_window", title))
