//! Everything that differs per operating system: the focused window, screenshots,
//! synthesizing the paste shortcut, and desktop integration (hotkey, autostart).
// STUB: replaced by the platform port.

use image::DynamicImage;

use crate::snapshot::WindowInfo;

/// Desktop context access. Linux: the GNOME helper extension over D-Bus, then fallbacks
/// (xprop / wmctrl, xdg-desktop-portal screenshots). macOS / Windows: native APIs.
pub struct Desktop;

impl Desktop {
    pub async fn connect() -> Self {
        Desktop
    }

    /// Linux: whether the GNOME helper extension answered. Always false elsewhere.
    pub fn helper_available(&self) -> bool {
        false
    }

    /// Name of the screenshot backend that last worked ("gnome-helper", "portal", "xcap", ...).
    pub fn screen_backend(&self) -> Option<String> {
        None
    }

    pub async fn focused_window(&self) -> WindowInfo {
        WindowInfo::default()
    }

    pub async fn list_windows(&self) -> Vec<WindowInfo> {
        vec![]
    }

    pub async fn screenshot(&self) -> Option<DynamicImage> {
        None
    }

    /// Linux/Wayland only: ask the helper extension to move (and keep above) a window by title.
    pub async fn place_window(&self, _title: &str, _x: i32, _y: i32, _above: bool) -> bool {
        false
    }
}

/// Ways this machine can synthesize the paste shortcut, best first.
pub fn paste_methods() -> Vec<&'static str> {
    vec![]
}

/// Press the paste shortcut in the focused window (Ctrl+V; Ctrl+Shift+V in Linux terminals;
/// Cmd+V on macOS). Blocking. Returns the method used, or None if nothing worked.
pub fn send_paste(_terminal: bool) -> Option<&'static str> {
    None
}

/// Start Fane at login (or stop). Returns a human-readable description of what was done.
pub fn set_autostart(_on: bool) -> anyhow::Result<String> {
    anyhow::bail!("not implemented")
}

/// Linux: install the bundled GNOME Shell helper extension. Returns a message for the user.
pub fn install_extension() -> anyhow::Result<String> {
    anyhow::bail!("not implemented")
}

/// Linux: bind a GNOME custom shortcut to `<exe> toggle`. Returns a message for the user.
pub fn setup_shortcut(_binding: &str) -> anyhow::Result<String> {
    anyhow::bail!("not implemented")
}

/// Extra lines for `fane doctor` (checks specific to this OS), as (ok, text).
pub fn doctor_checks() -> Vec<(bool, String)> {
    vec![]
}
