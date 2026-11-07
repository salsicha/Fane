"""Synthesize the paste shortcut in whatever window currently has focus.

Wayland gives ordinary clients no way to type into other apps, so we fall back
through: a virtual keyboard on /dev/uinput (needs write access), ydotool,
and xdotool (XWayland windows only).
"""

from __future__ import annotations

import fcntl
import logging
import os
import shutil
import struct
import subprocess
import time

log = logging.getLogger(__name__)

EV_SYN, EV_KEY, SYN_REPORT = 0, 1, 0
KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V = 29, 42, 47
UI_SET_EVBIT, UI_SET_KEYBIT = 0x40045564, 0x40045565
UI_DEV_CREATE, UI_DEV_DESTROY = 0x5501, 0x5502


class UInputKeyboard:
    """Tiny virtual keyboard using the legacy uinput write() setup API."""

    def __init__(self) -> None:
        self.fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
        fcntl.ioctl(self.fd, UI_SET_EVBIT, EV_KEY)
        for key in (KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V):
            fcntl.ioctl(self.fd, UI_SET_KEYBIT, key)
        name = b"fane-virtual-keyboard".ljust(80, b"\0")
        dev = name + struct.pack("HHHHi", 0x06, 0x1234, 0x5678, 1, 0) + b"\0" * (4 * 64 * 4)
        os.write(self.fd, dev)
        fcntl.ioctl(self.fd, UI_DEV_CREATE)
        time.sleep(0.3)  # give the compositor time to pick the device up

    def _emit(self, type_: int, code: int, value: int) -> None:
        now = time.time()
        os.write(self.fd, struct.pack("llHHi", int(now), int((now % 1) * 1e6), type_, code, value))

    def chord(self, *keys: int) -> None:
        for k in keys:
            self._emit(EV_KEY, k, 1)
            self._emit(EV_SYN, SYN_REPORT, 0)
            time.sleep(0.008)
        for k in reversed(keys):
            self._emit(EV_KEY, k, 0)
            self._emit(EV_SYN, SYN_REPORT, 0)
            time.sleep(0.008)

    def close(self) -> None:
        try:
            fcntl.ioctl(self.fd, UI_DEV_DESTROY)
        finally:
            os.close(self.fd)


_keyboard: UInputKeyboard | None = None


def available_methods() -> list[str]:
    methods = []
    if os.access("/dev/uinput", os.W_OK):
        methods.append("uinput")
    if shutil.which("ydotool"):
        methods.append("ydotool")
    if shutil.which("xdotool") and os.environ.get("DISPLAY"):
        methods.append("xdotool")
    return methods


def send_paste(terminal: bool = False) -> str | None:
    """Press Ctrl+V (Ctrl+Shift+V for terminals). Returns the method used, or None."""
    global _keyboard
    keys = (KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V) if terminal else (KEY_LEFTCTRL, KEY_V)
    for method in available_methods():
        try:
            if method == "uinput":
                if _keyboard is None:
                    _keyboard = UInputKeyboard()
                _keyboard.chord(*keys)
            elif method == "ydotool":
                down = [f"{k}:1" for k in keys]
                up = [f"{k}:0" for k in reversed(keys)]
                subprocess.run(["ydotool", "key", *down, *up], check=True, timeout=5,
                               capture_output=True)
            elif method == "xdotool":
                combo = "ctrl+shift+v" if terminal else "ctrl+v"
                subprocess.run(["xdotool", "key", "--clearmodifiers", combo], check=True, timeout=5)
            return method
        except Exception as e:
            log.warning("paste via %s failed: %s", method, e)
            if method == "uinput":
                _keyboard = None
    return None
