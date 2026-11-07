"""Configuration: ~/.config/fane/config.toml merged over built-in defaults."""

from __future__ import annotations

import copy
import os
import tomllib
from pathlib import Path
from typing import Any

CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "fane"
CONFIG_PATH = CONFIG_DIR / "config.toml"
STATE_DIR = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "fane"
RUNTIME_DIR = Path(os.environ.get("XDG_RUNTIME_DIR", f"/tmp/fane-{os.getuid()}"))
SOCKET_PATH = RUNTIME_DIR / "fane.sock"

DEFAULTS: dict[str, Any] = {
    "model": {
        "host": "http://127.0.0.1:11434",
        # Vision-language model served by Ollama. If it isn't installed, the first
        # installed model from `fallbacks` is used instead.
        "name": "gemma4",
        "fallbacks": ["gemma4", "minicpm-v", "qwen3-vl", "qwen2.5vl", "llava"],
        "temperature": 0.3,
        "keep_alive": "15m",
        # A modest context keeps the model fully on the GPU, which matters far more for
        # latency than long memory does here.
        "num_ctx": 8192,
        "max_image_side": 1280,
        "timeout": 300,
    },
    "suggestions": {
        "enabled": True,
        "interval_seconds": 25,
        # Ignore the screen unless it changed at least this much (0..1).
        "min_change": 0.015,
        "min_confidence": 0.65,
        "display_seconds": 18,
        # Base quiet period after a suggestion is dismissed; doubles on repeated dismissals.
        "cooldown_seconds": 90,
        "max_cooldown_seconds": 1800,
        "corner": "top-right",  # top-right | top-left | bottom-right | bottom-left
    },
    "privacy": {
        # Never capture the screen while one of these apps (wm_class / app id substring) is focused.
        "blocked_apps": ["keepassxc", "1password", "bitwarden", "seahorse", "gnome-keyring"],
        # ...or while the window title contains one of these.
        "blocked_titles": ["Private Browsing", "Incognito", "password"],
    },
    "hotkey": {
        # Bound through a GNOME custom shortcut by `fane setup-shortcut`.
        "binding": "<Control><Super>space",
    },
    "insert": {
        # Restore the previous clipboard text after pasting.
        "restore_clipboard": True,
        "paste_delay_ms": 180,
    },
    "connectors": {
        "notes_dirs": ["~/Notes", "~/Documents/Notes", "~/Obsidian"],
        "thunderbird_profiles": ["~/.thunderbird", "~/snap/thunderbird/common/.thunderbird"],
    },
    "delegate": {
        "claude_cmd": ["claude", "-p"],
        "codex_cmd": ["codex", "exec"],
        "timeout": 900,
        "cwd": "~",
    },
    # MCP servers (stdio) whose tools are offered to the model, e.g.:
    # [mcp.servers.filesystem]
    # command = "npx"
    # args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/Documents"]
    "mcp": {"servers": {}},
}

DEFAULT_TEMPLATE = """\
# Fane configuration. Anything omitted falls back to the built-in defaults
# (see src/fane/config.py for the full list).

[model]
name = "gemma4"            # any Ollama vision model, e.g. "minicpm-v", "qwen3-vl"
host = "http://127.0.0.1:11434"

[suggestions]
enabled = true
interval_seconds = 25
min_confidence = 0.65
corner = "top-right"

[privacy]
blocked_apps = ["keepassxc", "1password", "bitwarden", "seahorse"]
blocked_titles = ["Private Browsing", "Incognito", "password"]

[hotkey]
binding = "<Control><Super>space"

[connectors]
notes_dirs = ["~/Notes", "~/Documents/Notes"]

# Expose MCP server tools to the assistant:
# [mcp.servers.filesystem]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/Documents"]
"""


def _merge(base: dict, override: dict) -> dict:
    out = copy.deepcopy(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], value)
        else:
            out[key] = value
    return out


class Config:
    def __init__(self, data: dict[str, Any]):
        self.data = data

    def __getitem__(self, section: str) -> dict[str, Any]:
        return self.data[section]

    @classmethod
    def load(cls, path: Path = CONFIG_PATH) -> "Config":
        if not path.exists():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(DEFAULT_TEMPLATE)
        with path.open("rb") as f:
            user = tomllib.load(f)
        return cls(_merge(DEFAULTS, user))


def expand(path: str) -> Path:
    return Path(os.path.expandvars(os.path.expanduser(path)))
