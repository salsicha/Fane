//! Synthetic desktop screenshots for evals: deterministic, and contain no real user data.

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, PxScale};
use image::{Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;

pub const W: u32 = 1600;
pub const H: u32 = 900;

static SANS: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/fonts/DejaVuSans.ttf")).expect("embedded DejaVu Sans")
});
static MONO: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../../assets/fonts/DejaVuSansMono.ttf")).expect("embedded DejaVu Sans Mono")
});

#[derive(Clone, Copy)]
enum Face {
    Sans,
    Mono,
}

/// `0xrrggbb` -> pixel.
const fn c(hex: u32) -> Rgb<u8> {
    Rgb([(hex >> 16) as u8, (hex >> 8) as u8, hex as u8])
}

const WHITE: Rgb<u8> = c(0xffffff);

/// A tiny subset of PIL's `ImageDraw`: boxes are inclusive `[x0, y0, x1, y1]` like PIL's.
struct Draw {
    img: RgbImage,
}

impl Draw {
    fn put(&mut self, x: i32, y: i32, color: Rgb<u8>) {
        if (0..W as i32).contains(&x) && (0..H as i32).contains(&y) {
            self.img.put_pixel(x as u32, y as u32, color);
        }
    }

    fn rect(&mut self, [x0, y0, x1, y1]: [i32; 4], fill: Rgb<u8>) {
        for y in y0.max(0)..=y1.min(H as i32 - 1) {
            for x in x0.max(0)..=x1.min(W as i32 - 1) {
                self.img.put_pixel(x as u32, y as u32, fill);
            }
        }
    }

    /// Rectangle border `width` pixels thick, drawn inside the box.
    fn outline(&mut self, [x0, y0, x1, y1]: [i32; 4], color: Rgb<u8>, width: i32) {
        let w = width - 1;
        self.rect([x0, y0, x1, y0 + w], color);
        self.rect([x0, y1 - w, x1, y1], color);
        self.rect([x0, y0, x0 + w, y1], color);
        self.rect([x1 - w, y0, x1, y1], color);
    }

    /// Filled and/or outlined rounded rectangle (pixel centres tested against the shape).
    fn rounded(&mut self, b: [i32; 4], r: i32, fill: Option<Rgb<u8>>, outline: Option<(Rgb<u8>, i32)>) {
        let inside = |x: i32, y: i32, inset: i32| {
            let (x0, y0) = ((b[0] + inset) as f32, (b[1] + inset) as f32);
            let (x1, y1) = ((b[2] + 1 - inset) as f32, (b[3] + 1 - inset) as f32);
            let r = (r - inset).max(0) as f32;
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            if px < x0 || px > x1 || py < y0 || py > y1 {
                return false;
            }
            let cx = px.clamp(x0 + r, x1 - r);
            let cy = py.clamp(y0 + r, y1 - r);
            (px - cx).powi(2) + (py - cy).powi(2) <= r * r
        };
        for y in b[1]..=b[3] {
            for x in b[0]..=b[2] {
                if !inside(x, y, 0) {
                    continue;
                }
                match outline {
                    Some((color, width)) if !inside(x, y, width) => self.put(x, y, color),
                    _ => {
                        if let Some(f) = fill {
                            self.put(x, y, f);
                        }
                    }
                }
            }
        }
    }

    fn vline(&mut self, x: i32, y0: i32, y1: i32, color: Rgb<u8>, width: i32) {
        let left = x - (width - 1) / 2; // PIL puts the extra pixel on the right
        self.rect([left, y0, left + width - 1, y1], color);
    }

    /// Text with its top at the font's ascender line, like PIL's default "la" anchor.
    fn text(&mut self, (x, y): (i32, i32), s: &str, color: Rgb<u8>, face: Face, size: f32) {
        let font: &FontRef<'static> = match face {
            Face::Sans => &SANS,
            Face::Mono => &MONO,
        };
        // PIL sizes are pixels per em; ab_glyph scales are ascent-to-descent heights.
        let per_em = font.units_per_em().unwrap_or(2048.0);
        let scale = PxScale::from(size * font.height_unscaled() / per_em);
        draw_text_mut(&mut self.img, color, x, y, scale, font, s);
    }

    fn sans(&mut self, at: (i32, i32), s: &str, color: Rgb<u8>, size: f32) {
        self.text(at, s, color, Face::Sans, size);
    }

    fn mono(&mut self, at: (i32, i32), s: &str, color: Rgb<u8>, size: f32) {
        self.text(at, s, color, Face::Mono, size);
    }
}

const WI: i32 = W as i32;
const HI: i32 = H as i32;

/// GNOME-ish frame: top bar, dock, and a window title bar if `title` is non-empty.
fn desktop(title: &str) -> Draw {
    let mut img = RgbImage::new(W, H);
    for (_, y, p) in img.enumerate_pixels_mut() {
        // Ubuntu-like wallpaper gradient
        *p = Rgb([(44 + y / 12) as u8, 0, (30 + y / 20) as u8]);
    }
    let mut d = Draw { img };
    d.rect([0, 0, WI, 28], c(0x000000));
    d.sans((WI / 2 - 60, 5), "Thu 14:05", WHITE, 15.0);
    d.rect([0, 28, 64, HI], c(0x1d1d1d));
    for (i, color) in [0xe95420, 0x3584e4, 0x2ec27e, 0xf6d32d, 0x9141ac].into_iter().enumerate() {
        let i = i as i32;
        d.rounded([12, 44 + i * 60, 52, 84 + i * 60], 8, Some(c(color)), None);
    }
    if !title.is_empty() {
        d.rect([64, 28, WI, 64], c(0x303030));
        d.sans((84, 37), title, c(0xeeeeee), 16.0);
        d.rect([64, 64, WI, HI], WHITE);
    }
    d
}

pub fn email() -> RgbImage {
    let mut d = desktop("Inbox (3) - alex@example.com - Gmail — Mozilla Firefox");
    d.rect([64, 64, WI, 120], c(0xf6f8fc));
    d.sans((90, 80), "Gmail", c(0xd93025), 24.0);
    d.rect([64, 120, 300, HI], c(0xf6f8fc));
    for (i, label) in ["Inbox  3", "Starred", "Sent", "Drafts"].into_iter().enumerate() {
        d.sans((90, 140 + i as i32 * 36), label, c(0x202124), 17.0);
    }
    let mut y = 140;
    for (line, bold) in [
        ("Can we move the design review?", true),
        ("Dana Kim <dana@example.com>   to me   10:02 AM", false),
        ("", false),
        ("Hi Alex,", false),
        ("", false),
        ("Something came up this afternoon. Could we push today's design review", false),
        ("(2:30pm) to later this week? Tuesday or Thursday afternoon both work for me.", false),
        ("", false),
        ("Thanks,", false),
        ("Dana", false),
    ] {
        d.sans((330, y), line, c(0x202124), if bold { 24.0 } else { 19.0 });
        y += if bold { 44 } else { 32 };
    }
    d.rounded([330, y + 20, WI - 60, y + 200], 10, None, Some((c(0xc0c0c0), 2)));
    d.sans((350, y + 34), "Reply to Dana Kim", c(0x5f6368), 16.0);
    d.vline(350, y + 70, y + 96, c(0x1a73e8), 2); // text cursor
    d.img
}

pub fn code() -> RgbImage {
    let mut d = desktop("handlers.py — api — Visual Studio Code");
    d.rect([64, 64, WI, HI], c(0x1e1e1e));
    d.rect([64, 64, 320, HI], c(0x252526));
    for (i, name) in ["api/", "  handlers.py", "  models.py", "tests/", "  test_profile.py"].into_iter().enumerate() {
        d.mono((80, 80 + i as i32 * 26), name, c(0xcccccc), 15.0);
    }
    let src = [
        "def get_profile(request):",
        "    \"\"\"Return the profile for the logged-in user.\"\"\"",
        "    session = request.session",
        "    user = db.users.find(session['uid'])",
        "    prefs = load_prefs(user)",
        "    return {",
        "        'id': request.json['user_id'],",
        "        'name': user.name,",
        "        'prefs': prefs,",
        "    }",
    ];
    for (i, line) in src.into_iter().enumerate() {
        d.mono((340, 84 + i as i32 * 24), &format!("{:>3}  {line}", 36 + i), c(0xd4d4d4), 16.0);
    }
    d.rect([320, 560, WI, HI], c(0x181818));
    d.sans((340, 566), "TERMINAL", c(0xcccccc), 13.0);
    let out = [
        "$ pytest tests/test_profile.py",
        "F",
        "=================== FAILURES ===================",
        "_______________ test_get_profile _______________",
        "    def test_get_profile(client):",
        ">       resp = client.get('/profile')",
        "api/handlers.py:42: in get_profile",
        "    'id': request.json['user_id'],",
        "E   KeyError: 'user_id'",
        "========== 1 failed in 0.31s ==========",
    ];
    for (i, line) in out.into_iter().enumerate() {
        let red = line.starts_with("E ") || line.starts_with('F') || line.starts_with("=====");
        d.mono((340, 590 + i as i32 * 24), line, c(if red { 0xf14c4c } else { 0xcccccc }), 16.0);
    }
    d.img
}

pub fn chat() -> RgbImage {
    let mut d = desktop("#planning | Acme — Slack");
    d.rect([64, 64, 330, HI], c(0x3f0e40));
    for (i, ch) in ["# general", "# planning", "# random", "Priya Shah", "Sam Lee"].into_iter().enumerate() {
        let color = if ch == "# planning" { WHITE } else { c(0xcfc3cf) };
        d.sans((84, 90 + i as i32 * 32), ch, color, 17.0);
    }
    let msgs = [
        ("Sam Lee  11:40", "Reminder: quarterly reviews are next week."),
        ("Priya Shah  13:58", "Hey Alex, could you send me a short summary of the Q3 plan"),
        ("", "before 3pm? I want to put it in the board deck."),
    ];
    let mut y = 100;
    for (who, text) in msgs {
        if !who.is_empty() {
            d.sans((360, y), who, c(0x1d1c1d), 18.0);
            y += 30;
        }
        d.sans((360, y), text, c(0x1d1c1d), 19.0);
        y += 40;
    }
    d.rounded([360, HI - 120, WI - 40, HI - 40], 8, None, Some((c(0x868686), 2)));
    d.sans((380, HI - 95), "Message #planning", c(0x868686), 19.0);
    d.img
}

pub fn spreadsheet() -> RgbImage {
    let mut d = desktop("Marketing budget 2026 - Google Sheets — Mozilla Firefox");
    d.rect([64, 64, WI, 130], c(0xf8f9fa));
    d.mono((84, 100), "fx   =C5/B5", c(0x202124), 18.0);
    let cols = ["", "A", "B", "C", "D"];
    let rows = [
        ["1", "Channel", "2025 spend", "2026 spend", "Growth %"],
        ["2", "Search", "12,000", "15,000", "125%"],
        ["3", "Social", "8,000", "9,500", "119%"],
        ["4", "Events", "5,000", "4,000", "80%"],
        ["5", "Podcasts", "0", "3,000", "#DIV/0!"],
        ["6", "Total", "25,000", "31,500", "126%"],
    ];
    let (x0, y0, widths) = (84, 150, [50, 260, 220, 220, 220]);
    let xs: Vec<i32> = (0..=widths.len()).map(|i| x0 + widths[..i].iter().sum::<i32>()).collect();
    for (col, label) in cols.into_iter().enumerate() {
        d.sans((xs[col] + 10, y0), label, c(0x5f6368), 16.0);
    }
    for (r, row) in rows.iter().enumerate() {
        let y = y0 + 34 + r as i32 * 36;
        for (col, val) in row.iter().enumerate() {
            d.outline([xs[col], y, xs[col + 1], y + 36], c(0xe0e0e0), 1);
            let red = *val == "#DIV/0!";
            d.sans((xs[col] + 10, y + 8), val, c(if red { 0xd93025 } else { 0x202124 }), 17.0);
        }
    }
    d.outline([xs[4], y0 + 34 + 4 * 36, xs[5], y0 + 70 + 4 * 36], c(0x1a73e8), 3);
    d.img
}

pub fn idle() -> RgbImage {
    desktop("").img
}

pub fn photos() -> RgbImage {
    let mut d = desktop("Pictures — Files");
    let palette = [0x88c0d0, 0xa3be8c, 0xebcb8b, 0xd08770, 0xb48ead, 0x5e81ac, 0xbf616a, 0x8fbcbb];
    for i in 0..16 {
        let (x, y) = (120 + (i % 8) * 180, 110 + (i / 8) * 220);
        d.rect([x, y, x + 150, y + 150], c(palette[i as usize % palette.len()]));
        d.sans((x, y + 160), &format!("IMG_20260{}12_{}.jpg", i % 9 + 1, 1000 + i), c(0x333333), 13.0);
    }
    d.img
}

pub fn article() -> RgbImage {
    let mut d = desktop("Aurora - Wikipedia — Mozilla Firefox");
    d.sans((120, 90), "Aurora", c(0x202122), 40.0);
    let body = [
        "From Wikipedia, the free encyclopedia",
        "",
        "An aurora, also commonly known as the northern lights (aurora borealis)",
        "or southern lights (aurora australis), is a natural light display in Earth's",
        "sky, predominantly seen in high-latitude regions around the Arctic and Antarctic.",
        "Auroras are the result of disturbances in the magnetosphere caused by the",
        "solar wind. These disturbances alter the trajectories of charged particles",
        "in the magnetospheric plasma.",
    ];
    for (i, line) in body.into_iter().enumerate() {
        d.sans((120, 160 + i as i32 * 34), line, c(0x202122), 20.0);
    }
    d.img
}

/// One synthetic screen: how to draw it and the focused window it pretends to be.
#[derive(Debug, Clone, Copy)]
pub struct Scene {
    pub name: &'static str,
    pub title: &'static str,
    pub app: &'static str,
    pub render: fn() -> RgbImage,
}

pub fn scenes() -> Vec<Scene> {
    let s = |name, render, title, app| Scene { name, title, app, render };
    vec![
        s("email", email as fn() -> RgbImage, "Inbox (3) - alex@example.com - Gmail — Mozilla Firefox", "firefox"),
        s("code", code, "handlers.py — api — Visual Studio Code", "code"),
        s("chat", chat, "#planning | Acme — Slack", "slack"),
        s("spreadsheet", spreadsheet, "Marketing budget 2026 - Google Sheets — Mozilla Firefox", "firefox"),
        s("idle", idle, "", "gnome-shell"),
        s("photos", photos, "Pictures", "org.gnome.Nautilus"),
        s("article", article, "Aurora - Wikipedia — Mozilla Firefox", "firefox"),
    ]
}

pub fn scene(name: &str) -> Option<Scene> {
    scenes().into_iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes_render() {
        let all = scenes();
        assert_eq!(all.len(), 7);
        for s in all {
            let img = (s.render)();
            assert_eq!(img.dimensions(), (1600, 900), "{}", s.name);
        }
        assert!(scene("code").is_some() && scene("nope").is_none());
    }

    #[test]
    fn text_is_drawn() {
        // The title bar text must leave some light pixels on the dark bar.
        let img = code();
        let light = (84..600).filter(|&x| (37..60).any(|y| img.get_pixel(x, y).0[0] > 150)).count();
        assert!(light > 50, "{light}");
    }
}
