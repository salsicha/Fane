"""Configuration: ~/.config/fane/config.toml merged over built-in defaults."""

from __future__ import annotations

import copy
import json
import os
import re
import tomllib
from pathlib import Path
from typing import Any

CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "fane"
CONFIG_PATH = CONFIG_DIR / "config.toml"
STATE_DIR = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "fane"
RUNTIME_DIR = Path(os.environ.get("XDG_RUNTIME_DIR", f"/tmp/fane-{os.getuid()}"))
SOCKET_PATH = RUNTIME_DIR / "fane.sock"

# Recommended local vision models (Ollama tags), in default preference order.
RECOMMENDED_MODELS = [
    "gemma4:31b",                       # Gemma 4 31B, dense, native tools
    "qwen3.8:27b",                      # Qwen 3.8 27B, dense, native tools
    "openbmb/minicpm-v4.7:35b-a3b",     # MiniCPM-V 4.7 35B MoE (3B active), JSON tool calls
]

DEFAULTS: dict[str, Any] = {
    "model": {
        "host": "http://127.0.0.1:11434",
        # Vision-language model served by Ollama. If it isn't installed, the first
        # installed model from `fallbacks` is used instead. The three recommended
        # models are compared by `fane eval`.
        "name": "gemma4:31b",
        "fallbacks": [*RECOMMENDED_MODELS, "gemma4", "qwen3.8", "minicpm-v4.7",
                      "qwen3-vl", "minicpm-v", "qwen2.5vl", "llava"],
        # Optional different (e.g. faster) model just for proactive suggestions.
        # Note: two models only stay loaded together if both fit in memory.
        "suggest_name": "",
        # "auto" uses Ollama's native tool calling and falls back to a JSON protocol
        # for models without it (MiniCPM-V 4.7). Force with "native" or "json".
        "tool_mode": "auto",
        "temperature": 0.3,
        "keep_alive": "15m",
        # A modest context keeps the model fully on the GPU, which matters far more for
        # latency than long memory does here.
        "num_ctx": 8192,
        "json_max_tokens": 1536,
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
    # Fane's own email: a Google account from Settings → Online Accounts.
    "agent_mail": {
        # The one account Fane may use. Empty = email off. Pick with `fane mail use`
        # or the tray menu; Fane never falls back to another account.
        "account": "",
        "poll_seconds": 60,
        # Mail from these addresses that passes Google's DMARC check is treated as a
        # request from the user: Fane does it and replies automatically.
        "trusted_senders": [],
        "auto_reply_trusted": True,
        "max_auto_replies_per_hour": 20,
        # Let remote (emailed) requests look at the screen. Off: the reply could leak it.
        "remote_can_see_screen": False,
        # Tools withheld from emailed requests (nobody is at the desk to watch).
        "remote_exclude_tools": ["insert_text", "insert_image", "delegate",
                                 "fane_send_email", "fane_reply_email"],
        "signature": "— Fane (assistant)",
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
# Recommended: "gemma4:31b", "qwen3.8:27b", "openbmb/minicpm-v4.7:35b-a3b".
# Compare them on this machine with `fane eval`.
name = "gemma4:31b"
# suggest_name = "openbmb/minicpm-v4.7:35b-a3b"   # optional separate model for suggestions
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

[agent_mail]
# account = "my-assistant@gmail.com"     # must be in Settings → Online Accounts
# trusted_senders = ["me@example.com"]   # their mail runs as requests, auto-replied

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


def _toml_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    return json.dumps(value)  # JSON strings and string lists are valid TOML


def set_value(section: str, key: str, value: Any, path: Path = CONFIG_PATH) -> None:
    """Set one key in the user's config.toml, keeping their comments and layout."""
    if not path.exists():
        Config.load(path)
    lines = path.read_text().splitlines()
    line = f"{key} = {_toml_value(value)}"
    start = next((i for i, ln in enumerate(lines) if ln.strip() == f"[{section}]"), None)
    if start is None:
        lines += ["", f"[{section}]", line]
    else:
        end = next((i for i in range(start + 1, len(lines)) if lines[i].lstrip().startswith("[")),
                   len(lines))
        key_re = re.compile(rf"^\s*{re.escape(key)}\s*=")
        hit = next((i for i in range(start + 1, end) if key_re.match(lines[i])), None)
        if hit is None:
            lines.insert(start + 1, line)
        else:
            lines[hit] = line
    text = "\n".join(lines) + "\n"
    tomllib.loads(text)  # never write a config we can't read back
    path.write_text(text)


def expand(path: str) -> Path:
    return Path(os.path.expandvars(os.path.expanduser(path)))
