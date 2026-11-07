"""The command palette: summoned by hotkey, or by editing a suggestion."""

from __future__ import annotations

from typing import Any

from PySide6.QtCore import Qt, Signal
from PySide6.QtGui import QGuiApplication, QKeySequence, QShortcut
from PySide6.QtWidgets import (QFrame, QHBoxLayout, QLabel, QLayout, QLineEdit, QPushButton,
                               QTextBrowser, QVBoxLayout, QWidget)

from fane.ui.style import STYLE


class Palette(QWidget):
    submitted = Signal(str)
    insert_requested = Signal(str)
    hidden = Signal()

    WIDTH = 680

    def __init__(self) -> None:
        super().__init__(None, Qt.FramelessWindowHint | Qt.WindowStaysOnTopHint | Qt.Dialog)
        self.setWindowTitle("Fane")
        self.setAttribute(Qt.WA_TranslucentBackground)
        self.setStyleSheet(STYLE)
        self.job: int | None = None
        self.snapshot: Any = None
        self._answer = ""
        self._history: list[str] = []
        self._hist_pos = 0

        card = QFrame(objectName="card")
        card.setFixedWidth(self.WIDTH)
        outer = QVBoxLayout(self)
        outer.setSizeConstraint(QLayout.SetFixedSize)  # always hug the content
        outer.setContentsMargins(0, 0, 0, 0)
        outer.addWidget(card)
        lay = QVBoxLayout(card)
        lay.setContentsMargins(18, 12, 18, 14)
        lay.setSpacing(6)

        top = QHBoxLayout()
        top.addWidget(QLabel("✦ Fane", objectName="brand"))
        self.context = QLabel("", objectName="meta")
        top.addWidget(self.context, 1)
        lay.addLayout(top)

        self.input = QLineEdit(placeholderText="Ask, or tell me what to do here…")
        self.input.returnPressed.connect(self._submit)
        lay.addWidget(self.input)

        self.status = QLabel("", objectName="status")
        self.status.hide()
        lay.addWidget(self.status)

        self.answer = QTextBrowser(openExternalLinks=True)
        self.answer.setMaximumHeight(320)
        self.answer.hide()
        lay.addWidget(self.answer)

        self.buttons = QWidget()
        row = QHBoxLayout(self.buttons)
        row.setContentsMargins(0, 4, 0, 0)
        row.addStretch(1)
        copy = QPushButton("Copy")
        copy.clicked.connect(lambda: QGuiApplication.clipboard().setText(self._answer))
        insert = QPushButton("Insert at cursor", objectName="primary")
        insert.clicked.connect(lambda: self.insert_requested.emit(self._answer))
        row.addWidget(copy)
        row.addWidget(insert)
        self.buttons.hide()
        lay.addWidget(self.buttons)

        QShortcut(QKeySequence(Qt.Key_Escape), self, activated=self.hide)
        QShortcut(QKeySequence("Ctrl+Return"), self,
                  activated=lambda: self._answer and self.insert_requested.emit(self._answer))

    # --- showing ------------------------------------------------------------------
    def present(self, snapshot: Any, prefill: str = "") -> None:
        self.snapshot = snapshot
        label = snapshot.window.label if snapshot else ""
        self.context.setText(self.fontMetrics().elidedText(f"· {label}", Qt.ElideRight, 520) if label else "")
        self._reset()
        self.input.setText(prefill)
        self.input.end(False)
        geo = QGuiApplication.primaryScreen().availableGeometry()
        self.adjustSize()
        self.move(geo.x() + (geo.width() - self.WIDTH) // 2, geo.y() + int(geo.height() * 0.2))
        self.show()
        self.raise_()
        self.activateWindow()
        self.input.setFocus()

    def _reset(self) -> None:
        self.job = None
        self._answer = ""
        self.status.hide()
        self.answer.hide()
        self.buttons.hide()
        self.input.setEnabled(True)
        self.adjustSize()

    def hideEvent(self, e) -> None:
        super().hideEvent(e)
        self.hidden.emit()

    def hide_for_insert(self) -> None:
        if self.isVisible():
            self.hide()

    # --- input ---------------------------------------------------------------------
    def keyPressEvent(self, e) -> None:
        if e.key() in (Qt.Key_Up, Qt.Key_Down) and self._history:
            step = -1 if e.key() == Qt.Key_Up else 1
            self._hist_pos = max(0, min(len(self._history), self._hist_pos + step))
            self.input.setText(self._history[self._hist_pos] if self._hist_pos < len(self._history) else "")
            return
        super().keyPressEvent(e)

    def _submit(self) -> None:
        text = self.input.text().strip()
        if not text:
            return
        self._history = [h for h in self._history if h != text][-30:] + [text]
        self._hist_pos = len(self._history)
        self.answer.hide()
        self.buttons.hide()
        self.set_status("Thinking…")
        self.submitted.emit(text)

    # --- job feedback -----------------------------------------------------------------
    def set_status(self, text: str) -> None:
        self.status.setText(text)
        self.status.show()
        self.adjustSize()

    def show_answer(self, text: str, error: bool = False) -> None:
        self.status.hide()
        self._answer = "" if error else text
        self.answer.setMarkdown(text if not error else f"**Error:** {text}")
        self.answer.show()
        self.buttons.setVisible(not error and bool(text))
        doc_h = int(self.answer.document().size().height()) + 20
        self.answer.setFixedHeight(min(320, max(48, doc_h)))
        self.adjustSize()
