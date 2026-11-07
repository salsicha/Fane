"""The non-UI half of Fane. Lives on its own asyncio loop in a background thread."""

from __future__ import annotations

import asyncio
import logging
import threading
from concurrent.futures import Future
from typing import Any, Awaitable, Protocol

from fane.agent import Agent, AgentResult
from fane.config import Config
from fane.connectors import Tool, ToolRegistry, params
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
        self.suggester = Suggester(self.llm, cfg["suggestions"], cfg["model"]["max_image_side"])
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
        me = asyncio.current_task()
        for task in asyncio.all_tasks():
            if task is not me:
                task.cancel()

    # --- lifecycle ---------------------------------------------------------------
    async def _startup(self) -> None:
        await self.helper.connect()
        try:
            model = await self.llm.resolve_model()
            log.info("using model %s", model)
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
        add = self.tools.add
        S = lambda t, d="": {"type": t, "description": d}  # noqa: E731

        add(Tool("insert_text", "Insert text at the user's text cursor in the focused app "
                 "(as if pasted). Use for drafting, replying, filling in, rewriting.",
                 params(["text"], text=S("string", "The exact text to insert")),
                 lambda text: ui.insert_text(text), "Inserting"))
        add(Tool("insert_image", "Paste an image file at the user's cursor.",
                 params(["path"], path=S("string", "Absolute path to a PNG/JPEG file")),
                 lambda path: ui.insert_image(path), "Inserting image"))
        add(Tool("read_clipboard", "Read the current clipboard text.", params(),
                 lambda: ui.read_clipboard(), "Reading clipboard"))
        add(Tool("list_windows", "List the user's open windows/apps with titles.", params(),
                 lambda: list_windows(self.helper), "Looking at open apps"))
        add(Tool("search_notes", "Search the user's notes (Markdown/text folders). "
                 "Returns paths, titles and snippets.",
                 params(["query"], query=S("string", "Keywords"), limit=S("integer")),
                 notes.search, "Searching notes"))
        add(Tool("read_note", "Read a note's full text by path (from search_notes).",
                 params(["path"], path=S("string")), notes.read, "Reading note"))
        add(Tool("recent_emails", "Recent inbox email from the local Thunderbird client, newest first.",
                 params([], limit=S("integer", "How many (default 10)"),
                        query=S("string", "Optional filter text"),
                        include_body=S("boolean", "Return full bodies")),
                 mail.recent, "Checking email"))
        if agents := self.delegate.available():
            add(Tool("delegate", "Hand a difficult task (coding, debugging, multi-step research) to "
                     f"a stronger coding agent. Available: {', '.join(agents)}. Returns its final output.",
                     params(["agent", "task"], agent={"type": "string", "enum": agents},
                            task=S("string", "Complete, self-contained task description"),
                            cwd=S("string", "Working directory (optional)")),
                     self.delegate.run, "Delegating"))

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
        return {"model": self.llm.model, "helper": self.helper.available,
                "screen_backend": self.screen.backend, "paused": self.paused,
                "tools": sorted(self.tools.tools), "ready": self.ready}

