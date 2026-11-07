"""Qt application: wires the core to the palette, the overlay, the tray and the IPC socket."""

from __future__ import annotations

import json
import logging
import os
import signal
import subprocess
from concurrent.futures import Future

from PySide6.QtCore import QTimer
from PySide6.QtGui import QAction, QGuiApplication, QIcon
from PySide6.QtNetwork import QLocalServer, QLocalSocket
from PySide6.QtWidgets import QApplication, QMenu, QSystemTrayIcon

from fane.config import CONFIG_PATH, SOCKET_PATH, Config
from fane.core import Core
from fane.ui.bridge import UiBridge
from fane.ui.overlay import Overlay
from fane.ui.palette import Palette
from fane.ui.style import ICON_PATH

log = logging.getLogger(__name__)


class FaneApp:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.qt = QApplication.instance() or QApplication([])
        self.qt.setApplicationName("Fane")
        self.qt.setDesktopFileName("fane")
        self.qt.setQuitOnLastWindowClosed(False)
        self.qt.setWindowIcon(QIcon(str(ICON_PATH)))

        self.core = Core(cfg)
        self.bridge = UiBridge(self)
        self.palette = Palette()
        self.overlay = Overlay(cfg["suggestions"]["corner"], place=self._place_overlay)
        self.jobs: dict[int, tuple[str, str]] = {}  # job -> (origin, title)

        self.bridge.suggestion.connect(self._on_suggestion)
        self.bridge.status.connect(self._on_status)
        self.bridge.done.connect(self._on_done)
        self.bridge.notice.connect(lambda t: self.overlay.toast(t, 6))
        self.palette.submitted.connect(self._on_submit)
        self.palette.insert_requested.connect(self._insert)
        self.palette.hidden.connect(lambda: setattr(self.bridge, "busy", False))
        self.overlay.run_clicked.connect(self._run_suggestion)
        self.overlay.edit_clicked.connect(self._edit_suggestion)
        self.overlay.dismissed.connect(lambda s: s and self._suggester("dismissed"))
        self.overlay.expired.connect(lambda s: s and self._suggester("ignored"))
        self.overlay.insert_clicked.connect(self._insert)
        self.overlay.open_clicked.connect(self._open_answer)

        self._setup_tray()
        self._setup_ipc()

    # --- startup -------------------------------------------------------------------
    def run(self) -> int:
        self.core.start(self.bridge)
        log.info("Fane running on Qt platform %r", QGuiApplication.platformName())
        # Let Ctrl+C in a terminal stop us (Qt otherwise swallows SIGINT).
        signal.signal(signal.SIGINT, lambda *_: self.quit())
        signal.signal(signal.SIGTERM, lambda *_: self.quit())
        self._sig_timer = QTimer(interval=300, timeout=lambda: None)
        self._sig_timer.start()
        return self.qt.exec()

    def quit(self) -> None:
        self.core.stop()
        self.server.close()
        QLocalServer.removeServer(str(SOCKET_PATH))
        self.qt.quit()

    def _setup_tray(self) -> None:
        self.tray = QSystemTrayIcon(QIcon(str(ICON_PATH)))
        self.tray.setToolTip("Fane")
        menu = QMenu()
        menu.addAction("Ask Fane…", self.toggle_palette)
        menu.addAction("Suggest something now", self.core.wake)
        self.pause_action = QAction("Pause suggestions", menu, checkable=True)
        self.pause_action.setChecked(self.core.paused)
        self.pause_action.toggled.connect(self.set_paused)
        menu.addAction(self.pause_action)
        menu.addSeparator()
        menu.addAction("Settings…", lambda: subprocess.Popen(["xdg-open", str(CONFIG_PATH)]))
        menu.addAction("Quit", self.quit)
        self.tray.setContextMenu(menu)
        self.tray.activated.connect(lambda reason: reason == QSystemTrayIcon.Trigger
                                    and self.toggle_palette())
        self._tray_menu = menu
        self.tray.show()

    def _setup_ipc(self) -> None:
        QLocalServer.removeServer(str(SOCKET_PATH))
        self.server = QLocalServer()
        if not self.server.listen(str(SOCKET_PATH)):
            log.error("cannot listen on %s: %s", SOCKET_PATH, self.server.errorString())
        os.chmod(SOCKET_PATH, 0o600) if SOCKET_PATH.exists() else None
        self.server.newConnection.connect(self._on_ipc)

    def _on_ipc(self) -> None:
        while self.server.hasPendingConnections():
            sock: QLocalSocket = self.server.nextPendingConnection()
            sock.readyRead.connect(lambda s=sock: self._handle_ipc(s))

    def _handle_ipc(self, sock: QLocalSocket) -> None:
        if not sock.canReadLine():
            return
        try:
            req = json.loads(bytes(sock.readLine()).decode())
        except ValueError:
            req = {}
        cmd = req.get("cmd")
        reply: dict = {"ok": True}
        if cmd == "toggle":
            self.toggle_palette()
        elif cmd == "ask":
            self.open_palette(req.get("text", ""), submit=bool(req.get("text")))
        elif cmd == "suggest":
            self.core.wake()
        elif cmd in ("pause", "resume"):
            self.pause_action.setChecked(cmd == "pause")
        elif cmd == "status":
            reply.update(self.core.status())
        elif cmd == "quit":
            QTimer.singleShot(50, self.quit)
        else:
            reply = {"ok": False, "error": f"unknown command {cmd!r}"}
        sock.write((json.dumps(reply) + "\n").encode())
        sock.flush()
        sock.disconnectFromServer()

    # --- palette -------------------------------------------------------------------------
    def toggle_palette(self) -> None:
        if self.palette.isVisible():
            self.palette.hide()
        else:
            self.open_palette()

    def open_palette(self, prefill: str = "", submit: bool = False, answer: str = "") -> None:
        self.bridge.busy = True  # don't pop suggestions while the user is typing

        def ready(fut: Future) -> None:
            snap = None if fut.exception() else fut.result()
            self.bridge._invoke.emit(lambda _: show(snap), None)

        def show(snap) -> None:
            self.palette.present(snap, prefill)
            self._place_window("Fane", self.palette.x(), self.palette.y())
            if answer:
                self.palette.show_answer(answer)
            elif submit:
                self.palette._submit()

        self.core.submit(self.core.prepare_palette()).add_done_callback(ready)

    def _on_submit(self, text: str) -> None:
        job = self.core.ask(text, self.palette.snapshot)
        self.palette.job = job
        self.jobs[job] = ("palette", text)

    def _open_answer(self, text: str) -> None:
        self.open_palette(answer=text)

    # --- suggestions ------------------------------------------------------------------------
    def _on_suggestion(self, s) -> None:
        if self.palette.isVisible():
            return
        self.overlay.show_suggestion(s, self.cfg["suggestions"]["display_seconds"])

    def _run_suggestion(self, s) -> None:
        if s is None:
            return
        self._suggester("accepted")
        job = self.core.ask(s.prompt, self.core.last_snapshot)
        self.jobs[job] = ("overlay", s.title)
        self.overlay.show_working(job, s.title)

    def _edit_suggestion(self, s) -> None:
        if s is None:
            return
        self._suggester("accepted")
        self.open_palette(prefill=s.prompt)

    def _suggester(self, event: str) -> None:
        self.core.loop.call_soon_threadsafe(getattr(self.core.suggester, event))

    # --- job feedback --------------------------------------------------------------------------
    def _on_status(self, job: int, text: str) -> None:
        origin, _ = self.jobs.get(job, ("overlay", ""))
        if origin == "palette" and self.palette.isVisible() and self.palette.job == job:
            self.palette.set_status(text)
        else:
            self.overlay.update_status(job, text)

    def _on_done(self, job: int, result, error) -> None:
        origin, title = self.jobs.pop(job, ("overlay", ""))
        text = error or (result.text if result else "")
        if result and result.inserted:
            self.overlay.toast("Inserted ✓" + (f" — {text}" if text and len(text) < 80 else ""))
        elif origin == "palette" and self.palette.isVisible() and self.palette.job == job:
            self.palette.show_answer(text, error=bool(error))
        else:
            self.overlay.show_result(job, title or "Answer", text, error=bool(error))

    def _insert(self, text: str) -> None:
        if text:
            self.bridge._insert(Future(), text=text)

    def set_paused(self, paused: bool) -> None:
        self.core.paused = paused
        self.overlay.toast("Suggestions paused" if paused else "Suggestions on", 3)

    # --- native Wayland positioning via the shell helper ------------------------------------------
    def _place_window(self, title: str, x: int, y: int) -> None:
        if QGuiApplication.platformName() == "wayland" and self.core.helper.available:
            self.core.submit(self.core.helper.place_window(title, x, y, True))

    def _place_overlay(self, x: int, y: int) -> None:
        self._place_window("Fane Suggestion", x, y)
