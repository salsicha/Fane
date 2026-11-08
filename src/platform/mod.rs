//! Everything that differs per operating system: the focused window, screenshots,
//! synthesizing the paste shortcut, and desktop integration (hotkey, autostart).
//!
//! Public API (identical on every OS):
//! - [`Desktop`]: `connect`, `helper_available`, `screen_backend`, `focused_window`,
//!   `list_windows`, `screenshot`, `place_window`
//! - [`paste_methods`], [`send_paste`]
//! - [`set_autostart`], [`install_extension`], [`setup_shortcut`], [`doctor_checks`]
//!
//! Linux talks to the GNOME Shell helper extension over D-Bus and falls back to X11 tools,
//! grim and the xdg-desktop-portal. macOS and Windows use native APIs (xcap, enigo, ...).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("Fane supports Linux, macOS and Windows only");
