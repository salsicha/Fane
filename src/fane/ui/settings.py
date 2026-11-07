"""Settings window: pick the Ollama server Fane talks to."""

from __future__ import annotations

from typing import Any

from PySide6.QtCore import Qt, Signal
from PySide6.QtGui import QPalette
from PySide6.QtWidgets import (QDialog, QGroupBox, QHBoxLayout, QLabel, QLineEdit, QListWidget,
                               QListWidgetItem, QPushButton, QVBoxLayout)

from fane.discovery import OllamaServer
from fane.llm import choose_model


class SettingsDialog(QDialog):
    scan_requested = Signal()
    use_requested = Signal(str)
    edit_config = Signal()

    def __init__(self, model_cfg: dict[str, Any]) -> None:
        super().__init__(None)
        self.setWindowTitle("Fane Settings")
        self.setMinimumWidth(580)
        self.model_cfg = model_cfg
        self.current = model_cfg["host"]
        self.servers: list[OllamaServer] = []

        lay = QVBoxLayout(self)
        box = QGroupBox("Ollama server")
        lay.addWidget(box)
        b = QVBoxLayout(box)
        b.addWidget(QLabel("Fane sends its prompts and screenshots to this server. Pick one "
                           "found on your network, or type an address.", wordWrap=True))

        self.list = QListWidget()
        self.list.setMinimumHeight(170)
        self.list.setSpacing(2)
        self.list.currentItemChanged.connect(self._picked)
        self.list.itemDoubleClicked.connect(lambda _: self._use())
        b.addWidget(self.list)

        row = QHBoxLayout()
        self.status = QLabel("", wordWrap=True)
        row.addWidget(self.status, 1)
        self.scan_btn = QPushButton("Scan again", autoDefault=False)
        self.scan_btn.clicked.connect(self.scan)
        row.addWidget(self.scan_btn)
        b.addLayout(row)

        row = QHBoxLayout()
        row.addWidget(QLabel("Address:"))
        self.address = QLineEdit(placeholderText="192.168.1.20 or http://host:11434")
        self.address.returnPressed.connect(self._use)
        row.addWidget(self.address, 1)
        self.use_btn = QPushButton("Use this server", autoDefault=False)
        self.use_btn.clicked.connect(self._use)
        row.addWidget(self.use_btn)
        b.addLayout(row)

        note = QLabel("Traffic to another computer isn't encrypted, so only use servers on a "
                      "network you trust.", wordWrap=True)
        note.setEnabled(False)  # dimmed, in the system theme's colors
        b.addWidget(note)

        bottom = QHBoxLayout()
        edit = QPushButton("Edit config file…", autoDefault=False)
        edit.clicked.connect(self.edit_config)
        bottom.addWidget(edit)
        bottom.addStretch(1)
        close = QPushButton("Close", autoDefault=False)
        close.clicked.connect(self.close)
        bottom.addWidget(close)
        lay.addLayout(bottom)

    # --- driven by FaneApp -----------------------------------------------------------
    def present(self, current: str) -> None:
        self.current = current
        self.address.setText(current)
        self.show()
        self.raise_()
        self.activateWindow()
        if self.scan_btn.isEnabled():
            self.scan()

    def scan(self) -> None:
        self.scan_btn.setEnabled(False)
        self.status.setText("Looking for Ollama on this computer and your network…")
        self.scan_requested.emit()

    def show_servers(self, servers: list[OllamaServer]) -> None:
        self.servers = servers
        self.scan_btn.setEnabled(True)
        n = len(servers)
        self.status.setText(f"Found {n} Ollama server{'s' if n != 1 else ''}." if n else
                            "No Ollama servers found. Type an address if yours is on another "
                            "network or port.")
        self._render()

    def scan_failed(self, error: str) -> None:
        self.scan_btn.setEnabled(True)
        self.status.setText(f"Scan failed: {error}")

    def use_done(self, host: str, model: str) -> None:
        self.current = host
        self.use_btn.setEnabled(True)
        self.address.setText(host)
        self.status.setText(f"✓ Using {host} with {model}.")
        self._render()

    def use_failed(self, error: str) -> None:
        self.use_btn.setEnabled(True)
        self.status.setText(f"✗ {error}")

    # --- internals -------------------------------------------------------------------
    def _render(self) -> None:
        self.list.blockSignals(True)
        self.list.clear()
        dim = self.palette().color(QPalette.Disabled, QPalette.Text)
        for s in self.servers:
            model = choose_model(self.model_cfg, s.models)
            where = s.url.removeprefix("http://")
            title = where if s.label == s.address else f"{s.label}  —  {where}"
            in_use = s.matches(self.current)
            plural = "s" if len(s.models) != 1 else ""
            detail = f"Ollama {s.version} · {len(s.models)} model{plural} · "
            if not model:
                detail += "none of Fane's models"
            else:
                detail += f"{'using' if in_use else 'would use'} {model}"
            item = QListWidgetItem(f"{title}{'   (in use)' if in_use else ''}\n{detail}")
            item.setData(Qt.UserRole, s.url)
            if in_use:
                font = item.font()
                font.setBold(True)
                item.setFont(font)
            if not model:
                item.setForeground(dim)
                item.setToolTip(f"Pull a model there first, e.g. ollama pull {self.model_cfg['name']}")
            self.list.addItem(item)
            if in_use:
                self.list.setCurrentItem(item)
        self.list.blockSignals(False)

    def _picked(self, item: QListWidgetItem | None, _prev=None) -> None:
        if item is not None:
            self.address.setText(item.data(Qt.UserRole))

    def _use(self) -> None:
        url = self.address.text().strip()
        if not url or not self.use_btn.isEnabled():
            return
        self.use_btn.setEnabled(False)
        self.status.setText(f"Connecting to {url}…")
        self.use_requested.emit(url)
