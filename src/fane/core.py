"""The non-UI half of Fane. Lives on its own asyncio loop in a background thread."""

from __future__ import annotations

import asyncio
import logging
import threading
from concurrent.futures import Future
from typing import Any, Awaitable, Protocol

from fane.agent import Agent, AgentResult
from fane.config import Config
from fane.connectors import ToolRegistry
from fane.connectors.builtin import builtin_tools
from fane.connectors.delegate import Delegate
from fane.connectors.mail import ThunderbirdMail
from fane.connectors.mcp_client import MCPManager
from fane.connectors.notes import Notes
from fane.context.screen import ScreenCapture
from fane.context.shell import ShellHelper
from fane.context.windows import WindowInfo, focused_window, list_windows
from fane.llm import LLMError, Ollama
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
        return {"model": self.llm.model, "suggest_model": self.suggest_llm.model, "helper": self.helper.available,
                "screen_backend": self.screen.backend, "paused": self.paused,
                "tools": sorted(self.tools.tools), "ready": self.ready}

