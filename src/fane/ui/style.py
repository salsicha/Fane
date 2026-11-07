"""Shared look: a dark, translucent card similar to GNOME's notification bubbles."""

from __future__ import annotations

from pathlib import Path

ICON_PATH = Path(__file__).with_name("fane.svg")

ACCENT = "#8ab4f8"

STYLE = f"""
QFrame#card {{
    background: rgba(32, 33, 38, 238);
    border: 1px solid rgba(255, 255, 255, 30);
    border-radius: 14px;
}}
QLabel {{ color: #e8e8ea; background: transparent; }}
QLabel#brand {{ color: {ACCENT}; font-weight: 600; }}
QLabel#meta {{ color: #9a9ca3; font-size: 11px; }}
QLabel#title {{ font-size: 14px; font-weight: 600; }}
QLabel#body {{ color: #c6c7cc; }}
QLabel#status {{ color: #9a9ca3; font-size: 12px; }}
QLineEdit {{
    background: transparent; color: #f2f2f4; border: none;
    font-size: 18px; padding: 6px 2px; selection-background-color: {ACCENT};
}}
QTextBrowser {{
    background: rgba(255, 255, 255, 8); color: #e8e8ea; border: none;
    border-radius: 8px; padding: 8px; font-size: 13px;
}}
QPushButton {{
    background: rgba(255, 255, 255, 18); color: #e8e8ea; border: none;
    border-radius: 7px; padding: 5px 12px; font-size: 12px;
}}
QPushButton:hover {{ background: rgba(255, 255, 255, 34); }}
QPushButton#primary {{ background: {ACCENT}; color: #111; font-weight: 600; }}
QPushButton#primary:hover {{ background: #a8c7fa; }}
QPushButton#close {{ background: transparent; color: #9a9ca3; padding: 2px 6px; font-size: 14px; }}
QPushButton#close:hover {{ color: #fff; }}
"""
