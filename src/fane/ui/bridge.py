"""Thread-safe UIPort implementation: hops calls from the core thread onto the Qt thread."""

from __future__ import annotations

import asyncio
import logging
import threading
from concurrent.futures import Future
from typing import Any, Callable

from PySide6.QtCore import QObject, QTimer, Signal, Slot
from PySide6.QtGui import QGuiApplication, QImage

from fane.actions.keys import send_paste

log = logging.getLogger(__name__)


class UiBridge(QObject):
    _invoke = Signal(object, object)  # (callable, Future | None)

    # Re-emitted on the Qt thread for the widgets.
    suggestion = Signal(object)
    status = Signal(int, str)
    done = Signal(int, object, object)
    notice = Signal(str)
    mail = Signal(object)

    def __init__(self, app: Any):
        super().__init__()
        self.app = app  # FaneApp: palette, overlay, core
        self.busy = False
        self._invoke.connect(self._run)

    @Slot(object, object)
    def _run(self, fn: Callable[[Future | None], None], fut: Future | None) -> None:
        try:
            fn(fut)
        except Exception as e:
            log.exception("UI call failed")
            if fut and not fut.done():
                fut.set_exception(e)

    def _call(self, fn: Callable[[Future], None]):
        fut: Future = Future()
        self._invoke.emit(fn, fut)
        return asyncio.wrap_future(fut)

    # --- UIPort ---------------------------------------------------------------
    def show_suggestion(self, s) -> None:
        self.suggestion.emit(s)

    def job_status(self, job: int, text: str) -> None:
        self.status.emit(job, text)

    def job_done(self, job: int, result, error) -> None:
        self.done.emit(job, result, error)

    def notify(self, text: str) -> None:
        self.notice.emit(text)

    def is_busy(self) -> bool:
        # Plain attribute reads, safe enough across threads.
        return self.busy or self.app.overlay.suggestion is not None or self.app.overlay.confirming

    def show_mail(self, info) -> None:
        self.mail.emit(info)

    def confirm_send(self, details):
        """Ask the user (Send/Cancel card) before an email goes out; resolves to bool."""
        return self._call(lambda fut: self.app.overlay.show_confirm(
            details, lambda ok: fut.done() or fut.set_result(bool(ok))))

    def read_clipboard(self):
        def fn(fut: Future) -> None:
            fut.set_result(QGuiApplication.clipboard().text() or "(clipboard is empty)")
        return self._call(fn)

    def insert_text(self, text: str):
        return self._call(lambda fut: self._insert(fut, text=text))

    def insert_image(self, path: str):
        return self._call(lambda fut: self._insert(fut, image_path=path))

    # --- paste at cursor ----------------------------------------------------------
    def _insert(self, fut: Future, text: str | None = None, image_path: str | None = None) -> None:
        cfg = self.app.cfg["insert"]
        cb = QGuiApplication.clipboard()
        previous = cb.text() if cfg["restore_clipboard"] and text is not None else None
        if image_path:
            img = QImage(image_path)
            if img.isNull():
                fut.set_result(f"error: could not load image {image_path}")
                return
            cb.setImage(img)
        else:
            cb.setText(text or "")
        # Set the clipboard while we still have focus, then give focus back to the target app.
        self.app.palette.hide_for_insert()
        snap = self.app.core.last_snapshot
        terminal = bool(snap and snap.window.is_terminal)

        def paste() -> None:
            method = send_paste(terminal=terminal)
            self._invoke.emit(lambda _: finish(method), None)  # back to the Qt thread

        def finish(method: str | None) -> None:
            if method is None:
                self.notice.emit("Copied to clipboard — press Ctrl+V to paste.")
                fut.set_result("Could not type into the app, so the content was copied to the "
                               "clipboard and the user was asked to paste it.")
                return
            if previous is not None:
                QTimer.singleShot(600, self, lambda: cb.setText(previous))
            fut.set_result(f"inserted ({method})")

        QTimer.singleShot(cfg["paste_delay_ms"], self,
                          lambda: threading.Thread(target=paste, daemon=True).start())
