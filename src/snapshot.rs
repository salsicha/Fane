//! A moment of user context: the focused window plus (optionally) the screen.

use std::sync::Arc;
use std::time::SystemTime;

use image::{imageops::FilterType, DynamicImage};
use serde::{Deserialize, Serialize};

use crate::config::PrivacyCfg;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WindowInfo {
    #[serde(default)]
    pub title: String,
    /// App id, wm_class, or process name.
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub pid: i64,
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl WindowInfo {
    pub fn new(title: &str, app: &str) -> Self {
        Self { title: title.into(), app: app.into(), ..Default::default() }
    }

    pub fn label(&self) -> String {
        match (self.app.is_empty(), self.title.is_empty()) {
            (false, false) => format!("{} — {}", self.app, self.title),
            (true, false) => self.title.clone(),
            (false, true) => self.app.clone(),
            (true, true) => "unknown window".into(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        const TERMS: [&str; 12] = ["terminal", "kitty", "alacritty", "ptyxis", "konsole", "wezterm", "foot",
                                   "tilix", "terminator", "xterm", "ghostty", "console"];
        let app = self.app.to_lowercase();
        TERMS.iter().any(|t| app.contains(t))
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub window: WindowInfo,
    pub image: Option<Arc<DynamicImage>>,
    pub taken_at: SystemTime,
    /// True when privacy rules stopped us from looking at the screen.
    pub private: bool,
}

impl Snapshot {
    pub fn new(window: WindowInfo) -> Self {
        Self { window, image: None, taken_at: SystemTime::now(), private: false }
    }

    pub fn with_image(window: WindowInfo, image: DynamicImage) -> Self {
        Self { image: Some(Arc::new(image)), ..Self::new(window) }
    }
}

pub fn is_private(window: &WindowInfo, privacy: &PrivacyCfg) -> bool {
    let app = window.app.to_lowercase();
    let title = window.title.to_lowercase();
    privacy.blocked_apps.iter().any(|b| app.contains(&b.to_lowercase()))
        || privacy.blocked_titles.iter().any(|b| title.contains(&b.to_lowercase()))
}

/// Fraction (0..1) of a coarse grid that visibly changed between two screenshots.
pub fn change_ratio(a: Option<&DynamicImage>, b: Option<&DynamicImage>) -> f64 {
    let (Some(a), Some(b)) = (a, b) else { return 1.0 };
    if a.width() != b.width() || a.height() != b.height() {
        return 1.0;
    }
    let (w, h) = (160, 90);
    let da = a.to_luma8();
    let db = b.to_luma8();
    let da = image::imageops::resize(&da, w, h, FilterType::Triangle);
    let db = image::imageops::resize(&db, w, h, FilterType::Triangle);
    let changed = da.pixels().zip(db.pixels()).filter(|(p, q)| p.0[0].abs_diff(q.0[0]) > 20).count();
    changed as f64 / (w * h) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn label_and_terminal() {
        assert_eq!(WindowInfo::new("Inbox", "thunderbird").label(), "thunderbird — Inbox");
        assert_eq!(WindowInfo::default().label(), "unknown window");
        assert!(WindowInfo::new("", "org.gnome.Ptyxis").is_terminal());
        assert!(!WindowInfo::new("", "firefox").is_terminal());
    }

    #[test]
    fn change_ratio_detects_half_screen() {
        let a = DynamicImage::ImageRgb8(RgbImage::from_pixel(320, 180, Rgb([255, 255, 255])));
        let mut bi = RgbImage::from_pixel(320, 180, Rgb([255, 255, 255]));
        assert_eq!(change_ratio(Some(&a), Some(&DynamicImage::ImageRgb8(bi.clone()))), 0.0);
        for y in 0..180 {
            for x in 0..160 {
                bi.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let r = change_ratio(Some(&a), Some(&DynamicImage::ImageRgb8(bi)));
        assert!(r > 0.4 && r < 0.6, "{r}");
        assert_eq!(change_ratio(None, Some(&a)), 1.0);
    }

    #[test]
    fn privacy_rules() {
        let rules = PrivacyCfg { blocked_apps: vec!["keepassxc".into()], blocked_titles: vec!["Private Browsing".into()] };
        assert!(is_private(&WindowInfo::new("db.kdbx", "org.keepassxc.KeePassXC"), &rules));
        assert!(is_private(&WindowInfo::new("Mozilla Firefox Private Browsing", "firefox"), &rules));
        assert!(!is_private(&WindowInfo::new("Inbox", "thunderbird"), &rules));
    }

    #[test]
    fn window_info_from_helper_json() {
        let w: WindowInfo = serde_json::from_str(r#"{"title":"T","app":"a","pid":7,"fullscreen":false}"#).unwrap();
        assert_eq!((w.title.as_str(), w.app.as_str(), w.pid), ("T", "a", 7));
        assert_eq!(w.extra["fullscreen"], false);
    }
}
