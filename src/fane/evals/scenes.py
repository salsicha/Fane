"""Synthetic desktop screenshots for evals: deterministic, and contain no real user data."""

from __future__ import annotations

from typing import Callable

from PIL import Image, ImageDraw, ImageFont

W, H = 1600, 900


def _font(size: int) -> ImageFont.FreeTypeFont:
    for name in ("DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return ImageFont.load_default(size=size)


def _mono(size: int) -> ImageFont.FreeTypeFont:
    for name in ("DejaVuSansMono.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return _font(size)


def _desktop(title: str = "") -> tuple[Image.Image, ImageDraw.ImageDraw]:
    """GNOME-ish frame: top bar, dock, and a window title bar if `title` is given."""
    img = Image.new("RGB", (W, H), "#2c001e")
    d = ImageDraw.Draw(img)
    for y in range(H):  # Ubuntu-like wallpaper gradient
        d.line([(0, y), (W, y)], fill=(44 + y // 12, 0, 30 + y // 20))
    d.rectangle([0, 0, W, 28], fill="#000000")
    d.text((W // 2 - 60, 5), "Thu 14:05", fill="white", font=_font(15))
    d.rectangle([0, 28, 64, H], fill="#1d1d1d")
    for i, color in enumerate(["#e95420", "#3584e4", "#2ec27e", "#f6d32d", "#9141ac"]):
        d.rounded_rectangle([12, 44 + i * 60, 52, 84 + i * 60], 8, fill=color)
    if title:
        d.rectangle([64, 28, W, 64], fill="#303030")
        d.text((84, 37), title, fill="#eeeeee", font=_font(16))
        d.rectangle([64, 64, W, H], fill="white")
    return img, d


def email() -> Image.Image:
    img, d = _desktop("Inbox (3) - alex@example.com - Gmail — Mozilla Firefox")
    d.rectangle([64, 64, W, 120], fill="#f6f8fc")
    d.text((90, 80), "Gmail", fill="#d93025", font=_font(24))
    d.rectangle([64, 120, 300, H], fill="#f6f8fc")
    for i, label in enumerate(["Inbox  3", "Starred", "Sent", "Drafts"]):
        d.text((90, 140 + i * 36), label, fill="#202124", font=_font(17))
    f, y = _font(19), 140
    for line, bold in [("Can we move the design review?", True),
                       ("Dana Kim <dana@example.com>   to me   10:02 AM", False), ("", False),
                       ("Hi Alex,", False), ("", False),
                       ("Something came up this afternoon. Could we push today's design review", False),
                       ("(2:30pm) to later this week? Tuesday or Thursday afternoon both work for me.", False),
                       ("", False), ("Thanks,", False), ("Dana", False)]:
        d.text((330, y), line, fill="#202124", font=_font(24) if bold else f)
        y += 44 if bold else 32
    d.rounded_rectangle([330, y + 20, W - 60, y + 200], 10, outline="#c0c0c0", width=2)
    d.text((350, y + 34), "Reply to Dana Kim", fill="#5f6368", font=_font(16))
    d.line([(350, y + 70), (350, y + 96)], fill="#1a73e8", width=2)  # text cursor
    return img


def code() -> Image.Image:
    img, d = _desktop("handlers.py — api — Visual Studio Code")
    d.rectangle([64, 64, W, H], fill="#1e1e1e")
    d.rectangle([64, 64, 320, H], fill="#252526")
    for i, name in enumerate(["api/", "  handlers.py", "  models.py", "tests/", "  test_profile.py"]):
        d.text((80, 80 + i * 26), name, fill="#cccccc", font=_mono(15))
    m = _mono(16)
    src = ["def get_profile(request):", '    """Return the profile for the logged-in user."""',
           "    session = request.session", "    user = db.users.find(session['uid'])",
           "    prefs = load_prefs(user)", "    return {",
           "        'id': request.json['user_id'],", "        'name': user.name,",
           "        'prefs': prefs,", "    }"]
    for i, line in enumerate(src):
        d.text((340, 84 + i * 24), f"{36 + i:>3}  {line}", fill="#d4d4d4", font=m)
    d.rectangle([320, 560, W, H], fill="#181818")
    d.text((340, 566), "TERMINAL", fill="#cccccc", font=_font(13))
    out = ["$ pytest tests/test_profile.py", "F", "=================== FAILURES ===================",
           "_______________ test_get_profile _______________",
           "    def test_get_profile(client):",
           ">       resp = client.get('/profile')",
           "api/handlers.py:42: in get_profile",
           "    'id': request.json['user_id'],",
           "E   KeyError: 'user_id'",
           "========== 1 failed in 0.31s =========="]
    for i, line in enumerate(out):
        color = "#f14c4c" if line.startswith(("E ", "F", "=====")) else "#cccccc"
        d.text((340, 590 + i * 24), line, fill=color, font=m)
    return img


def chat() -> Image.Image:
    img, d = _desktop("#planning | Acme — Slack")
    d.rectangle([64, 64, 330, H], fill="#3f0e40")
    for i, ch in enumerate(["# general", "# planning", "# random", "Priya Shah", "Sam Lee"]):
        d.text((84, 90 + i * 32), ch, fill="white" if ch == "# planning" else "#cfc3cf", font=_font(17))
    f = _font(19)
    msgs = [("Sam Lee  11:40", "Reminder: quarterly reviews are next week."),
            ("Priya Shah  13:58", "Hey Alex, could you send me a short summary of the Q3 plan"),
            ("", "before 3pm? I want to put it in the board deck.")]
    y = 100
    for who, text in msgs:
        if who:
            d.text((360, y), who, fill="#1d1c1d", font=_font(18))
            y += 30
        d.text((360, y), text, fill="#1d1c1d", font=f)
        y += 40
    d.rounded_rectangle([360, H - 120, W - 40, H - 40], 8, outline="#868686", width=2)
    d.text((380, H - 95), "Message #planning", fill="#868686", font=f)
    return img


def spreadsheet() -> Image.Image:
    img, d = _desktop("Marketing budget 2026 - Google Sheets — Mozilla Firefox")
    d.rectangle([64, 64, W, 130], fill="#f8f9fa")
    d.text((84, 100), "fx   =C5/B5", fill="#202124", font=_mono(18))
    cols = ["", "A", "B", "C", "D"]
    rows = [["1", "Channel", "2025 spend", "2026 spend", "Growth %"],
            ["2", "Search", "12,000", "15,000", "125%"],
            ["3", "Social", "8,000", "9,500", "119%"],
            ["4", "Events", "5,000", "4,000", "80%"],
            ["5", "Podcasts", "0", "3,000", "#DIV/0!"],
            ["6", "Total", "25,000", "31,500", "126%"]]
    x0, y0, widths = 84, 150, [50, 260, 220, 220, 220]
    xs = [x0 + sum(widths[:i]) for i in range(len(widths) + 1)]
    for c, label in enumerate(cols):
        d.text((xs[c] + 10, y0), label, fill="#5f6368", font=_font(16))
    for r, row in enumerate(rows):
        y = y0 + 34 + r * 36
        for c, val in enumerate(row):
            d.rectangle([xs[c], y, xs[c + 1], y + 36], outline="#e0e0e0")
            red = val == "#DIV/0!"
            d.text((xs[c] + 10, y + 8), val, fill="#d93025" if red else "#202124", font=_font(17))
    d.rectangle([xs[4], y0 + 34 + 4 * 36, xs[5], y0 + 70 + 4 * 36], outline="#1a73e8", width=3)
    return img


def idle() -> Image.Image:
    img, _ = _desktop()
    return img


def photos() -> Image.Image:
    img, d = _desktop("Pictures — Files")
    palette = ["#88c0d0", "#a3be8c", "#ebcb8b", "#d08770", "#b48ead", "#5e81ac", "#bf616a", "#8fbcbb"]
    for i in range(16):
        x, y = 120 + (i % 8) * 180, 110 + (i // 8) * 220
        d.rectangle([x, y, x + 150, y + 150], fill=palette[i % len(palette)])
        d.text((x, y + 160), f"IMG_20260{i % 9 + 1}12_{1000 + i}.jpg", fill="#333333", font=_font(13))
    return img


def article() -> Image.Image:
    img, d = _desktop("Aurora - Wikipedia — Mozilla Firefox")
    d.text((120, 90), "Aurora", fill="#202122", font=_font(40))
    body = ["From Wikipedia, the free encyclopedia", "",
            "An aurora, also commonly known as the northern lights (aurora borealis)",
            "or southern lights (aurora australis), is a natural light display in Earth's",
            "sky, predominantly seen in high-latitude regions around the Arctic and Antarctic.",
            "Auroras are the result of disturbances in the magnetosphere caused by the",
            "solar wind. These disturbances alter the trajectories of charged particles",
            "in the magnetospheric plasma."]
    for i, line in enumerate(body):
        d.text((120, 160 + i * 34), line, fill="#202122", font=_font(20))
    return img


SCENES: dict[str, tuple[Callable[[], Image.Image], str, str]] = {
    # name: (render, focused window title, app)
    "email": (email, "Inbox (3) - alex@example.com - Gmail — Mozilla Firefox", "firefox"),
    "code": (code, "handlers.py — api — Visual Studio Code", "code"),
    "chat": (chat, "#planning | Acme — Slack", "slack"),
    "spreadsheet": (spreadsheet, "Marketing budget 2026 - Google Sheets — Mozilla Firefox", "firefox"),
    "idle": (idle, "", "gnome-shell"),
    "photos": (photos, "Pictures", "org.gnome.Nautilus"),
    "article": (article, "Aurora - Wikipedia — Mozilla Firefox", "firefox"),
}
