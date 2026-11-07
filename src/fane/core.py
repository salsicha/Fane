"""The non-UI half of Fane. Lives on its own asyncio loop in a background thread."""

from __future__ import annotations

import asyncio
import logging
import threading
from concurrent.futures import Future
from typing import Any, Awaitable, Protocol

from fane.accounts import AccountError, OnlineAccounts
from fane.agent import Agent, AgentResult
from fane.config import Config, set_value
from fane.connectors import ToolRegistry
from fane.connectors.builtin import builtin_tools
from fane.connectors.delegate import Delegate
from fane.connectors.mail import ThunderbirdMail
from fane.connectors.mcp_client import MCPManager
from fane.connectors.notes import Notes
from fane.context.screen import ScreenCapture
from fane.context.shell import ShellHelper
from fane.context.windows import WindowInfo, focused_window, list_windows
from fane.discovery import NotOllama, OllamaServer, discover, normalize_host, probe
from fane.llm import LLMError, Ollama, choose_model
from fane.mail.gmail import GmailClient, MailError
from fane.mail.mailbox import AgentMailbox, mail_tool_names
from fane.snapshot import Snapshot, is_private
from fane.suggester import Suggester, Suggestion

log = logging.getLogger(__name__)

OWN_TITLES = ("Fane",)


class UIPort(Protocol):
    """What the core needs from the UI. Implementations must be thread-safe."""

    def insert_text(self, text: str) -> Awaitable[str]: ...
    def insert_image(self, path: str) -> Awaitable[str]: ...
    def read_clipboard(self) -> Awaitable[str]: ...
    def show_suggestion(self, s: Suggestion) -> None: ...
    def job_status(self, job: int, text: str) -> None: ...
    def job_done(self, job: int, result: AgentResult | None, error: str | None) -> None: ...
    def notify(self, text: str) -> None: ...
    def is_busy(self) -> bool: ...
    def show_mail(self, info: dict[str, Any]) -> None: ...
    def confirm_send(self, details: dict[str, Any]) -> Awaitable[bool]: ...


class Core:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.ui: UIPort | None = None
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self.loop.run_forever, name="fane-core", daemon=True)
        self.helper = ShellHelper()
        self.screen = ScreenCapture(self.helper)
        self.llm = Ollama(cfg["model"])
        self.tools = ToolRegistry()
        self.mcp = MCPManager(cfg["mcp"].get("servers", {}))
        self.delegate = Delegate(cfg["delegate"])
        self.agent = Agent(self.llm, self.tools, cfg["model"])
        suggest_llm = self.llm
        if cfg["model"].get("suggest_name"):
            suggest_llm = Ollama({**cfg["model"], "name": cfg["model"]["suggest_name"]})
        self.suggest_llm = suggest_llm
        self.suggester = Suggester(suggest_llm, cfg["suggestions"], cfg["model"]["max_image_side"])
        self.paused = not cfg["suggestions"]["enabled"]
        self.ready = False
        self.last_snapshot: Snapshot | None = None
        self._job_seq = 0
        self._agent_running = False
        self._wake = asyncio.Event()
        self.accounts = OnlineAccounts()
        self.mailbox: AgentMailbox | None = None
        self.mail_error: str | None = None

    # --- thread plumbing --------------------------------------------------------
    def start(self, ui: UIPort) -> None:
        self.ui = ui
        self.thread.start()
        self.submit(self._startup())

    def submit(self, coro) -> Future:
        return asyncio.run_coroutine_threadsafe(coro, self.loop)

    def stop(self) -> None:
        if not self.thread.is_alive():
            return
        try:
            self.submit(self._shutdown()).result(timeout=8)
        except Exception:
            pass
        self.loop.call_soon_threadsafe(self.loop.stop)

    async def _shutdown(self) -> None:
        if self.mailbox:
            await self.mailbox.stop()
        await self.mcp.stop()
        await self.llm.aclose()
        if self.suggest_llm is not self.llm:
            await self.suggest_llm.aclose()
        me = asyncio.current_task()
        for task in asyncio.all_tasks():
            if task is not me:
                task.cancel()

    # --- lifecycle ---------------------------------------------------------------
    async def _startup(self) -> None:
        await self.helper.connect()
        for llm in {id(self.llm): self.llm, id(self.suggest_llm): self.suggest_llm}.values():
            try:
                log.info("using model %s", await llm.resolve_model())
            except LLMError as e:
                log.error("%s", e)
                self.ui.notify(str(e))
        self._register_builtin_tools()
        await self.mcp.start(self.tools)
        await self.setup_mail()
        self.ready = True
        asyncio.create_task(self._suggest_loop())

    def _register_builtin_tools(self) -> None:
        c = self.cfg["connectors"]
        notes = Notes(c["notes_dirs"])
        mail = ThunderbirdMail(c["thunderbird_profiles"])
        ui = self.ui
        handlers = {
            "insert_text": lambda text: ui.insert_text(text),
            "insert_image": lambda path: ui.insert_image(path),
            "read_clipboard": lambda: ui.read_clipboard(),
            "list_windows": lambda: list_windows(self.helper),
            "search_notes": notes.search,
            "read_note": notes.read,
            "recent_emails": mail.recent,
            "delegate": self.delegate.run,
        }
        for tool in builtin_tools(handlers, self.delegate.available()):
            self.tools.add(tool)

    # --- Fane's own email ---------------------------------------------------------
    async def setup_mail(self) -> None:
        """(Re)connect to the one Online Accounts mailbox the user selected, if any."""
        if self.mailbox:
            await self.mailbox.stop()
            self.mailbox = None
        for name in mail_tool_names():
            self.tools.remove(name)
        self.mail_error = None
        mcfg = self.cfg["agent_mail"]
        address = mcfg.get("account", "").strip()
        if not address:
            return
        try:
            acc = await self.accounts.find(address)
            if acc is None:
                raise AccountError(f"{address} isn't in Settings → Online Accounts")
            client = GmailClient(acc.email, lambda: self.accounts.access_token(acc),
                                 acc.imap_host, acc.smtp_host)
            mailbox = AgentMailbox(client, mcfg, self)
            await mailbox.start()
        except (AccountError, MailError, OSError) as e:
            self.mail_error = str(e)
            log.warning("agent email disabled: %s", e)
            self.ui.notify(f"Fane's email is off: {e}")
            return
        self.mailbox = mailbox
        for tool in mailbox.tools():
            self.tools.add(tool)

    async def reload_mail_config(self) -> None:
        """Re-read [agent_mail] from disk (after `fane mail trust` etc.) and reconnect."""
        fresh = Config.load()["agent_mail"]
        self.cfg["agent_mail"].clear()
        self.cfg["agent_mail"].update(fresh)
        await self.setup_mail()

    async def select_mail_account(self, address: str) -> None:
        set_value("agent_mail", "account", address)
        self.cfg["agent_mail"]["account"] = address
        await self.setup_mail()
        if address and self.mailbox:
            self.ui.notify(f"Fane now uses {address} for email")
        elif not address:
            self.ui.notify("Fane's email is off")

    # --- which Ollama server ---------------------------------------------------------
    async def discover_ollama(self) -> list[OllamaServer]:
        """Ollama servers on this computer and the local network (plus the current one)."""
        return await discover(extra=[self.llm.host])

    async def use_ollama(self, url: str) -> str:
        """Switch to the Ollama server at `url` and save it. Returns the model it will use.

        Nothing changes unless the server answers and has one of Fane's models installed.
        """
        url = normalize_host(url)
        server = await probe(url)
        mcfg = self.cfg["model"]
        if not choose_model(mcfg, server.models):
            raise NotOllama(f"{url} has none of Fane's models. Pull one there first, e.g. "
                            f"ollama pull {mcfg['name']}")
        set_value("model", "host", url)
        for llm in {id(self.llm): self.llm, id(self.suggest_llm): self.suggest_llm}.values():
            llm.set_host(url)
            await llm.resolve_model()
        log.info("switched to Ollama at %s (model %s)", url, self.llm.model)
        self.ui.notify(f"Fane now uses Ollama at {url} ({self.llm.model})")
        return self.llm.model

    # MailHost: the mailbox calls back into these.
    async def run_remote(self, prompt: str) -> str:
        exclude = set(self.cfg["agent_mail"]["remote_exclude_tools"])
        agent = Agent(self.llm, self.tools.without(exclude), self.cfg["model"])
        if self.cfg["agent_mail"]["remote_can_see_screen"]:
            snap = await self.snapshot()
        else:
            snap = Snapshot(window=WindowInfo(title="(the user is away; screen not shared)"))
        self._agent_running = True
        try:
            result = await agent.run(prompt, snap, self.suggester.history, lambda s: None)
        finally:
            self._agent_running = False
        return result.text

    def show_mail(self, info: dict[str, Any]) -> None:
        self.ui.show_mail(info)

    def notify(self, text: str) -> None:
        self.ui.notify(text)

    def confirm_send(self, details: dict[str, Any]):
        return self.ui.confirm_send(details)

    # --- context -------------------------------------------------------------------
    async def snapshot(self, with_screen: bool = True) -> Snapshot:
        window = await focused_window(self.helper)
        snap = Snapshot(window=window)
        if is_private(window, self.cfg["privacy"]):
            snap.private = True
        elif with_screen:
            snap.image = await self.screen.grab()
        return snap

    async def prepare_palette(self) -> Snapshot:
        """Capture context right before the palette covers the screen."""
        try:
            snap = await asyncio.wait_for(self.snapshot(), 1.5)
        except asyncio.TimeoutError:
            snap = self.last_snapshot or Snapshot(window=WindowInfo())
        self.last_snapshot = snap
        return snap

    # --- proactive loop ------------------------------------------------------------
    def wake(self) -> None:
        self.loop.call_soon_threadsafe(self._wake.set)

    async def _suggest_loop(self) -> None:
        interval = self.cfg["suggestions"]["interval_seconds"]
        while True:
            try:
                await asyncio.wait_for(self._wake.wait(), interval)
                forced = True
            except asyncio.TimeoutError:
                forced = False
            self._wake.clear()
            if (self.paused and not forced) or self._agent_running or self.ui.is_busy():
                continue
            try:
                await self._suggest_once(forced)
            except LLMError as e:
                log.warning("suggestion failed: %s", e)
            except Exception:
                log.exception("suggestion step crashed")

    async def _suggest_once(self, forced: bool) -> None:
        snap = await self.snapshot()
        if snap.private or snap.window.title in OWN_TITLES:
            return
        self.last_snapshot = snap
        sug = await self.suggester.step(snap, force=forced)
        if sug and not self.ui.is_busy():
            self.ui.show_suggestion(sug)
        elif forced and not sug:
            self.ui.notify("Nothing to suggest right now.")

    # --- requests from the UI --------------------------------------------------------
    def ask(self, prompt: str, snap: Snapshot | None) -> int:
        self._job_seq += 1
        job = self._job_seq
        self.submit(self._run_job(job, prompt, snap))
        return job

    async def _run_job(self, job: int, prompt: str, snap: Snapshot | None) -> None:
        self._agent_running = True
        try:
            snap = snap or self.last_snapshot or await self.snapshot()
            result = await self.agent.run(prompt, snap, self.suggester.history,
                                          lambda s: self.ui.job_status(job, s))
            self.ui.job_done(job, result, None)
        except LLMError as e:
            self.ui.job_done(job, None, str(e))
        except Exception as e:
            log.exception("job failed")
            self.ui.job_done(job, None, f"Something went wrong: {e}")
        finally:
            self._agent_running = False

    def status(self) -> dict[str, Any]:
        return {"ollama_host": self.llm.host,
                "model": self.llm.model, "suggest_model": self.suggest_llm.model,
                "mail_account": self.mailbox.address if self.mailbox else None,
                "mail_error": self.mail_error or (self.mailbox.error if self.mailbox else None),
                "helper": self.helper.available,
                "screen_backend": self.screen.backend, "paused": self.paused,
                "tools": sorted(self.tools.tools), "ready": self.ready}

