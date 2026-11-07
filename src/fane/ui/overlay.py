"""Small, non-focusable card in the notification corner for suggestions and results."""

from __future__ import annotations

from typing import Callable

from PySide6.QtCore import QEasingCurve, QPropertyAnimation, Qt, QTimer, Signal
from PySide6.QtGui import QGuiApplication
from PySide6.QtWidgets import (QFrame, QHBoxLayout, QLabel, QLayout, QPushButton, QScrollArea,
                               QVBoxLayout, QWidget)

from fane.ui.style import STYLE

MARGIN = 14
WIDTH = 380


class Overlay(QWidget):
    run_clicked = Signal(object)        # Suggestion
    edit_clicked = Signal(object)       # Suggestion
    dismissed = Signal(object)          # Suggestion | None
    expired = Signal(object)            # Suggestion | None
    insert_clicked = Signal(str)
    open_clicked = Signal(str)

    def __init__(self, corner: str = "top-right", place: Callable[[int, int], None] | None = None):
        flags = (Qt.Tool | Qt.FramelessWindowHint | Qt.WindowStaysOnTopHint
                 | Qt.WindowDoesNotAcceptFocus | Qt.X11BypassWindowManagerHint)
        super().__init__(None, flags)
        self.setWindowTitle("Fane Suggestion")
        self.setAttribute(Qt.WA_TranslucentBackground)
        self.setAttribute(Qt.WA_ShowWithoutActivating)
        self.setStyleSheet(STYLE)
        self.corner = corner
        self.place = place  # used on native Wayland, where we can't position ourselves
        self.suggestion = None
        self.result_text = ""
        self.job: int | None = None

        self.card = QFrame(objectName="card")
        self.card.setFixedWidth(WIDTH)
        outer = QVBoxLayout(self)
        outer.setSizeConstraint(QLayout.SetFixedSize)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.addWidget(self.card)
        lay = QVBoxLayout(self.card)
        lay.setContentsMargins(14, 10, 10, 12)
        lay.setSpacing(4)

        head = QHBoxLayout()
        head.addWidget(QLabel("✦ Fane", objectName="brand"))
        self.meta = QLabel("", objectName="meta")
        head.addWidget(self.meta, 1)
        close = QPushButton("✕", objectName="close")
        close.setCursor(Qt.PointingHandCursor)
        close.clicked.connect(self._dismiss)
        head.addWidget(close)
        lay.addLayout(head)

        self.title = QLabel("", objectName="title", wordWrap=True)
        lay.addWidget(self.title)
        self.body = QLabel("", objectName="body", wordWrap=True)
        self.body.setTextInteractionFlags(Qt.TextSelectableByMouse)
        self.scroll = QScrollArea(widgetResizable=True, frameShape=QFrame.NoFrame)
        self.scroll.setStyleSheet("background: transparent;")
        self.scroll.setWidget(self.body)
        self.scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        lay.addWidget(self.scroll)

        self.row = QHBoxLayout()
        self.row.setContentsMargins(0, 6, 0, 0)
        self.row.addStretch(1)
        lay.addLayout(self.row)
        self._buttons: list[QPushButton] = []

        self.timer = QTimer(self, singleShot=True, timeout=self._expire)
        self.fade = QPropertyAnimation(self, b"windowOpacity", self)
        self.fade.setEasingCurve(QEasingCurve.OutCubic)
        self.fade.finished.connect(self._fade_done)
        self._after_fade: Callable[[], None] | None = None

    # --- modes ----------------------------------------------------------------------
    def show_suggestion(self, s, seconds: int) -> None:
        self.suggestion, self.job, self.result_text = s, None, ""
        self.meta.setText(_short(s.window))
        self.title.setText(s.title)
        self._set_body(s.prompt, max_h=64, markdown=False)
        self._set_buttons([("Edit", lambda: self._act(self.edit_clicked)),
                           ("Run", lambda: self._act(self.run_clicked), True)])
        self._present(seconds)

    def show_working(self, job: int, title: str, status: str = "Working…") -> None:
        self.suggestion, self.job = None, job
        self.meta.setText("")
        self.title.setText(title)
        self._set_body(status, max_h=40, markdown=False)
        self._set_buttons([])
        self._present(0)

    def update_status(self, job: int, status: str) -> None:
        if self.job == job and self.isVisible():
            self.body.setText(status)

    def show_result(self, job: int, title: str, text: str, error: bool = False) -> None:
        if self.job not in (None, job):
            return
        self.suggestion, self.job, self.result_text = None, job, text
        self.meta.setText("")
        self.title.setText("Couldn't do that" if error else title)
        self._set_body(text, max_h=240)
        buttons = [] if error else [
            ("Open", lambda: self._act_text(self.open_clicked)),
            ("Copy", lambda: (QGuiApplication.clipboard().setText(self.result_text), self._fade_out())),
            ("Insert", lambda: self._act_text(self.insert_clicked), True)]
        self._set_buttons(buttons)
        self._present(45)

    def toast(self, text: str, seconds: int = 4) -> None:
        if self.suggestion is not None and self.isVisible():
            return  # don't clobber a pending suggestion with a notice
        self.suggestion, self.job = None, None
        self.meta.setText("")
        self.title.setText(text)
        self._set_body("", max_h=0)
        self._set_buttons([])
        self._present(seconds)

    # --- internals -------------------------------------------------------------------
    def _set_body(self, text: str, max_h: int, markdown: bool = True) -> None:
        self.body.setTextFormat(Qt.MarkdownText if markdown else Qt.PlainText)
        self.body.setText(text)
        self.scroll.setVisible(bool(text))
        self.body.adjustSize()
        self.scroll.setFixedHeight(min(max_h, self.body.sizeHint().height() + 4))

    def _set_buttons(self, specs) -> None:
        for b in self._buttons:
            self.row.removeWidget(b)
            b.deleteLater()
        self._buttons = []
        for spec in specs:
            label, fn, *primary = spec
            b = QPushButton(label, objectName="primary" if primary else "")
            b.setCursor(Qt.PointingHandCursor)
            b.clicked.connect(fn)
            self.row.addWidget(b)
            self._buttons.append(b)

    def _present(self, seconds: int) -> None:
        self.adjustSize()
        geo = QGuiApplication.primaryScreen().availableGeometry()
        x = geo.right() - WIDTH - MARGIN if "right" in self.corner else geo.left() + MARGIN
        y = geo.bottom() - self.height() - MARGIN if "bottom" in self.corner else geo.top() + MARGIN
        self.move(x, y)
        if not self.isVisible():
            self.setWindowOpacity(0.0)
            self.show()
            if self.place:
                self.place(x, y)
        self._animate(1.0)  # also cancels a fade-out that is still running
        if seconds:
            self.timer.start(seconds * 1000)
        else:
            self.timer.stop()

    def _animate(self, to: float, then: Callable[[], None] | None = None) -> None:
        self.fade.stop()
        self._after_fade = then
        self.fade.setDuration(180)
        self.fade.setStartValue(self.windowOpacity())
        self.fade.setEndValue(to)
        self.fade.start()

    def _fade_done(self) -> None:
        then, self._after_fade = self._after_fade, None
        if then:
            then()

    def _fade_out(self) -> None:
        self.timer.stop()
        self._animate(0.0, self.hide)

    def _act(self, signal) -> None:
        s = self.suggestion
        self.suggestion = None
        self._fade_out()
        signal.emit(s)

    def _act_text(self, signal) -> None:
        text = self.result_text
        self._fade_out()
        signal.emit(text)

    def _dismiss(self) -> None:
        s = self.suggestion
        self.suggestion = None
        self._fade_out()
        self.dismissed.emit(s)

    def _expire(self) -> None:
        if self.underMouse():
            self.timer.start(3000)
            return
        s = self.suggestion
        self.suggestion = None
        self._fade_out()
        self.expired.emit(s)


def _short(label: str, n: int = 40) -> str:
    app = label.split(" — ")[0]
    return f"· {app[:n]}" if app else ""
