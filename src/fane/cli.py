"""`fane` command line: start the daemon, talk to it, and set up the desktop."""

from __future__ import annotations

import argparse
import asyncio
import ast
import json
import logging
import os
import shutil
import socket
import subprocess
import sys
from pathlib import Path

from fane.config import CONFIG_PATH, SOCKET_PATH, STATE_DIR, Config

EXT_UUID = "fane-helper@salsicha.github.io"
SHORTCUT_PATH = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fane/"
MEDIA_KEYS = "org.gnome.settings-daemon.plugins.media-keys"


def send(cmd: str, **kw) -> dict | None:
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(5)
            s.connect(str(SOCKET_PATH))
            s.sendall((json.dumps({"cmd": cmd, **kw}) + "\n").encode())
            data = b""
            while not data.endswith(b"\n"):
                chunk = s.recv(65536)
                if not chunk:
                    break
                data += chunk
        return json.loads(data or b"{}")
    except (OSError, ValueError):
        return None


def setup_logging(verbose: bool) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    logging.basicConfig(
        level=logging.DEBUG if verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        handlers=[logging.StreamHandler(), logging.FileHandler(STATE_DIR / "fane.log")],
    )
    for noisy in ("httpx", "httpcore", "mcp"):
        logging.getLogger(noisy).setLevel(logging.WARNING)


def cmd_run(args) -> int:
    if send("status") is not None:
        print("Fane is already running. Use `fane toggle` to open it.")
        return 0
    setup_logging(args.verbose)
    # Prefer XWayland: it lets the overlay sit in a screen corner and stay on top.
    # Falls back to native Wayland if the xcb plugin can't load (needs libxcb-cursor0).
    if os.environ.get("WAYLAND_DISPLAY") and "QT_QPA_PLATFORM" not in os.environ:
        os.environ["QT_QPA_PLATFORM"] = "xcb;wayland"
    from fane.ui.app import FaneApp

    return FaneApp(Config.load()).run()


def cmd_simple(args) -> int:
    kw = {"text": " ".join(args.text)} if getattr(args, "text", None) else {}
    reply = send(args.cmd, **kw)
    if reply is None:
        if args.cmd in ("toggle", "ask"):
            # Hotkey pressed while not running: start in the background, then open.
            subprocess.Popen([sys.executable, "-m", "fane", "run"], start_new_session=True,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            print("Starting Fane…")
            return 0
        print("Fane is not running (start it with `fane run`).", file=sys.stderr)
        return 1
    if args.cmd == "status":
        print(json.dumps(reply, indent=2))
    return 0 if reply.get("ok", True) else 1


def _gsettings(*a: str) -> str:
    return subprocess.run(["gsettings", *a], capture_output=True, text=True, check=True).stdout.strip()


def cmd_setup_shortcut(args) -> int:
    cfg = Config.load()
    binding = args.binding or cfg["hotkey"]["binding"]
    exe = shutil.which("fane") or f"{sys.executable} -m fane"
    raw = _gsettings("get", MEDIA_KEYS, "custom-keybindings")
    current = [] if raw.startswith("@as") else ast.literal_eval(raw)
    if SHORTCUT_PATH not in current:
        current.append(SHORTCUT_PATH)
        _gsettings("set", MEDIA_KEYS, "custom-keybindings", str(current))
    schema = f"{MEDIA_KEYS}.custom-keybinding:{SHORTCUT_PATH}"
    _gsettings("set", schema, "name", "Fane")
    _gsettings("set", schema, "command", f"{exe} toggle")
    _gsettings("set", schema, "binding", binding)
    print(f"Bound {binding} → `{exe} toggle`. Change it in Settings → Keyboard → Custom Shortcuts.")
    return 0


def cmd_install_extension(args) -> int:
    src = Path(__file__).resolve().parents[2] / "gnome-extension" / EXT_UUID
    if not src.is_dir():
        src = Path(sys.prefix) / "share" / "fane" / EXT_UUID
    if not src.is_dir():
        print("Could not find the gnome-extension directory.", file=sys.stderr)
        return 1
    dest = Path.home() / ".local/share/gnome-shell/extensions" / EXT_UUID
    if dest.exists() or dest.is_symlink():
        if dest.is_symlink() or dest.is_file():
            dest.unlink()
        else:
            shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(src, dest)
    r = subprocess.run(["gnome-extensions", "enable", EXT_UUID], capture_output=True, text=True)
    print(f"Installed {EXT_UUID} to {dest}.")
    if r.returncode != 0:
        print("On Wayland GNOME only loads new extensions after you log out and back in.\n"
              f"Then run: gnome-extensions enable {EXT_UUID}")
    else:
        print("Enabled. If `fane doctor` still can't see it, log out and back in.")
    return 0


def cmd_autostart(args) -> int:
    path = Path.home() / ".config/autostart/fane.desktop"
    if args.state == "off":
        path.unlink(missing_ok=True)
        print("Autostart disabled.")
        return 0
    exe = shutil.which("fane") or f"{sys.executable} -m fane"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"[Desktop Entry]\nType=Application\nName=Fane\nExec={exe} run\n"
                    "X-GNOME-Autostart-enabled=true\nNoDisplay=true\n")
    print(f"Autostart enabled ({path}).")
    return 0


def cmd_doctor(args) -> int:
    from fane.actions.keys import available_methods
    from fane.context.screen import ScreenCapture
    from fane.context.shell import ShellHelper
    from fane.context.windows import focused_window
    from fane.config import RECOMMENDED_MODELS
    from fane.llm import LLMError, Ollama, find_installed

    cfg = Config.load()
    ok = lambda b: "✓" if b else "✗"  # noqa: E731

    async def check() -> None:
        print(f"config: {CONFIG_PATH}")
        print(f"session: {os.environ.get('XDG_SESSION_TYPE')} / {os.environ.get('XDG_CURRENT_DESKTOP')}")
        llm = Ollama(cfg["model"])
        try:
            print(f"{ok(True)} model: {await llm.resolve_model()}")
            installed = await llm.installed_models()
            for name in RECOMMENDED_MODELS:
                have = find_installed(name, installed)
                print(f"  {ok(bool(have))} recommended {name}" + ("" if have else f" — ollama pull {name}"))
        except LLMError as e:
            print(f"{ok(False)} model: {e}")
        helper = ShellHelper()
        await helper.connect()
        print(f"{ok(helper.available)} GNOME helper extension"
              + ("" if helper.available else " (run `fane install-extension`)"))
        win = await focused_window(helper)
        print(f"{ok(bool(win.title or win.app))} focused window: {win.label}")
        if args.screenshot:
            img = await ScreenCapture(helper).grab()
            print(f"{ok(img is not None)} screenshot: {img.size if img else 'no backend worked'}")
        else:
            print("  screenshot: skipped (pass --screenshot to test capture)")
        methods = available_methods()
        print(f"{ok(bool(methods))} paste methods: {', '.join(methods) or 'none — will copy to clipboard only'}")
        for tool in ("claude", "codex"):
            print(f"{ok(bool(shutil.which(tool)))} {tool} CLI for delegation")
        xcb = Path("/usr/lib/x86_64-linux-gnu/libxcb-cursor.so.0").exists()
        print(f"{ok(xcb)} libxcb-cursor0 (lets the overlay sit in a corner via XWayland)"
              + ("" if xcb else " — sudo apt install libxcb-cursor0"))
        mail = cfg["agent_mail"].get("account", "")
        print(f"{ok(bool(mail))} Fane's email: {mail or 'off (fane mail accounts / fane mail use)'}")
        servers = cfg["mcp"].get("servers", {})
        print(f"  MCP servers configured: {', '.join(servers) or 'none'}")
        print(f"  running: {'yes' if send('status') else 'no'}")
        await llm.aclose()

    asyncio.run(check())
    return 0


def cmd_eval(args) -> int:
    import datetime as dt

    from fane.config import RECOMMENDED_MODELS
    from fane.evals.runner import EvalRunner, save
    from fane.evals.scenarios import AGENT_TASKS, SUGGEST_CASES

    if args.list:
        for c in SUGGEST_CASES:
            print(f"suggest  {c.scene:<18} expect: {c.expect or 'not scored'}")
        for t in AGENT_TASKS:
            print(f"agent    {t.name:<18} {t.prompt}")
        return 0
    cfg = Config.load()
    if args.num_ctx:
        cfg["model"]["num_ctx"] = args.num_ctx
    if args.temperature is not None:
        cfg["model"]["temperature"] = args.temperature
    if args.tool_mode:
        cfg["model"]["tool_mode"] = args.tool_mode
    models = args.models or RECOMMENDED_MODELS
    out = Path(args.out) if args.out else STATE_DIR / "evals" / dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    if send("status") is not None:
        print("Note: Fane is running; pause it (`fane pause`) so it doesn't compete for the GPU.")
    runner = EvalRunner(cfg, repeat=args.repeat, only=args.only, select=args.filter, timeout=args.timeout)
    res = asyncio.run(runner.run(models))
    report = save(res, out)
    print("\n" + report.read_text())
    print(f"Saved {report} (full transcripts in results.json, screens in scenes/).")
    return 0 if any(m["status"] == "ok" for m in res["models"]) else 1


def cmd_mail(args) -> int:
    from fane.accounts import AccountError, OnlineAccounts
    from fane.config import set_value

    cfg = Config.load()
    mcfg = cfg["agent_mail"]
    current = mcfg.get("account", "")

    async def accounts():
        return await OnlineAccounts().google_accounts()

    if args.action in ("accounts", "use"):
        try:
            accs = asyncio.run(accounts())
        except AccountError as e:
            print(e, file=sys.stderr)
            return 1
    if args.action == "accounts":
        if not accs:
            print("No Google accounts in Settings → Online Accounts. Add the agent's account there.")
        for a in accs:
            mark = "●" if a.email.lower() == current.lower() else "○"
            note = "  (mail switched off in Settings)" if a.mail_disabled else ""
            print(f"{mark} {a.email}{note}")
        print(f"\nFane uses: {current or 'nothing (email off)'}. Change with `fane mail use ADDRESS`.")
        return 0
    if args.action in ("use", "off"):
        address = "" if args.action == "off" else args.value
        if address and not any(a.email.lower() == address.lower() for a in accs):
            print(f"{address} isn't in Settings → Online Accounts (see `fane mail accounts`).", file=sys.stderr)
            return 1
        if send("mail_use", address=address) is None:
            set_value("agent_mail", "account", address)
        print(f"Fane's email account: {address}" if address else "Fane's email is off.")
        return 0
    if args.action in ("trust", "untrust"):
        trusted = [t for t in mcfg.get("trusted_senders", []) if t.lower() != args.value.lower()]
        if args.action == "trust":
            trusted.append(args.value.lower())
        set_value("agent_mail", "trusted_senders", trusted)
        send("mail_reload")
        print("Trusted senders:", ", ".join(trusted) or "none")
        return 0
    # status
    reply = send("status")
    print(f"account:  {current or '(off)'}")
    print(f"trusted:  {', '.join(mcfg.get('trusted_senders', [])) or 'none'}")
    if reply:
        print(f"running:  watching {reply.get('mail_account') or 'nothing'}"
              + (f" — error: {reply['mail_error']}" if reply.get("mail_error") else ""))
    else:
        print("running:  no (start Fane with `fane`)")
    return 0


def main(argv: list[str] | None = None) -> None:
    p = argparse.ArgumentParser(prog="fane", description="Unobtrusive local desktop assistant.")
    sub = p.add_subparsers(dest="cmd")

    r = sub.add_parser("run", help="start Fane (default)")
    r.add_argument("-v", "--verbose", action="store_true")
    r.set_defaults(fn=cmd_run)
    for name, helptext in [("toggle", "show/hide the prompt box"), ("suggest", "suggest something now"),
                           ("pause", "pause proactive suggestions"), ("resume", "resume suggestions"),
                           ("status", "show daemon status"), ("quit", "stop the daemon")]:
        sub.add_parser(name, help=helptext).set_defaults(fn=cmd_simple)
    a = sub.add_parser("ask", help="open the prompt box and run a request")
    a.add_argument("text", nargs="+")
    a.set_defaults(fn=cmd_simple)
    s = sub.add_parser("setup-shortcut", help="bind a GNOME keyboard shortcut to `fane toggle`")
    s.add_argument("--binding", help="e.g. '<Control><Super>space'")
    s.set_defaults(fn=cmd_setup_shortcut)
    sub.add_parser("install-extension", help="install the GNOME Shell helper extension") \
        .set_defaults(fn=cmd_install_extension)
    au = sub.add_parser("autostart", help="start Fane when you log in")
    au.add_argument("state", choices=["on", "off"])
    au.set_defaults(fn=cmd_autostart)
    ml = sub.add_parser("mail", help="Fane's own email account (from Settings → Online Accounts)")
    ml.add_argument("action", choices=["accounts", "use", "off", "trust", "untrust", "status"])
    ml.add_argument("value", nargs="?", help="email address (for use / trust / untrust)")
    ml.set_defaults(fn=cmd_mail)
    d = sub.add_parser("doctor", help="check what works on this machine")
    d.add_argument("--screenshot", action="store_true", help="also test screen capture")
    d.set_defaults(fn=cmd_doctor)

    e = sub.add_parser("eval", help="compare local models on Fane's prompts and tools")
    e.add_argument("--models", nargs="+", help="Ollama tags (default: the three recommended models)")
    e.add_argument("--repeat", type=int, default=1, help="run each case N times")
    e.add_argument("--only", choices=["suggest", "agent"], help="run one half of the suite")
    e.add_argument("--filter", help="only cases whose name contains this")
    e.add_argument("--timeout", type=float, default=600, help="seconds per case")
    e.add_argument("--num-ctx", type=int, help="override [model] num_ctx")
    e.add_argument("--temperature", type=float, help="override [model] temperature")
    e.add_argument("--tool-mode", choices=["auto", "native", "json"], help="override [model] tool_mode")
    e.add_argument("--out", help="output directory (default ~/.local/state/fane/evals/<time>)")
    e.add_argument("--list", action="store_true", help="list the cases and exit")
    e.set_defaults(fn=cmd_eval)

    args = p.parse_args(argv)
    if args.cmd == "mail" and args.action in ("use", "trust", "untrust") and not args.value:
        p.error(f"fane mail {args.action} needs an email address")
    if not args.cmd:
        args = p.parse_args(["run", *(argv or sys.argv[1:])])
    sys.exit(args.fn(args))
