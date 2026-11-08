//! macOS and Windows: native APIs for the focused window (active-win-pos-rs), the window
//! list and screenshots (xcap), the paste shortcut (enigo) and login items (auto-launch).
//!
//! On macOS, synthesizing keys needs the Accessibility permission and screenshots need
//! Screen Recording (System Settings → Privacy & Security).

use std::sync::Mutex;

use image::DynamicImage;
use tracing::{info, warn};

use crate::snapshot::WindowInfo;

/// Desktop context access. macOS / Windows: native APIs; there is no helper.
pub struct Desktop {
    backend: Mutex<Option<String>>,
}

impl Desktop {
    pub async fn connect() -> Self {
        Desktop { backend: Mutex::new(None) }
    }

    /// Linux: whether the GNOME helper extension answered. Always false elsewhere.
    pub fn helper_available(&self) -> bool {
        false
    }

    /// Name of the screenshot backend that last worked ("gnome-helper", "portal", "xcap", ...).
    pub fn screen_backend(&self) -> Option<String> {
        self.backend.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub async fn focused_window(&self) -> WindowInfo {
        let active = tokio::task::spawn_blocking(active_win_pos_rs::get_active_window).await;
        match active {
            Ok(Ok(w)) => WindowInfo {
                title: w.title,
                app: w.app_name,
                pid: i64::try_from(w.process_id).unwrap_or(0),
                ..Default::default()
            },
            _ => WindowInfo::default(),
        }
    }

    pub async fn list_windows(&self) -> Vec<WindowInfo> {
        tokio::task::spawn_blocking(list_windows_blocking).await.unwrap_or_default()
    }

    pub async fn screenshot(&self) -> Option<DynamicImage> {
        let img = match tokio::task::spawn_blocking(capture_primary_monitor).await {
            Ok(Ok(img)) => img,
            Ok(Err(e)) => {
                warn!("screenshot via xcap failed: {e}");
                return None;
            }
            Err(e) => {
                warn!("screenshot task failed: {e}");
                return None;
            }
        };
        let mut last = self.backend.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_deref() != Some("xcap") {
            info!("screenshot backend: xcap");
            *last = Some("xcap".into());
        }
        Some(img)
    }

    /// Linux/Wayland only: ask the helper extension to move (and keep above) a window by title.
    /// Here the UI positions its own windows, so this is a no-op.
    pub async fn place_window(&self, _title: &str, _x: i32, _y: i32, _above: bool) -> bool {
        false
    }
}

fn list_windows_blocking() -> Vec<WindowInfo> {
    let windows = match xcap::Window::all() {
        Ok(w) => w,
        Err(e) => {
            warn!("listing windows failed: {e}");
            return vec![];
        }
    };
    windows
        .iter()
        .filter(|w| !w.is_minimized().unwrap_or(false))
        .filter_map(|w| {
            let title = w.title().ok().filter(|t| !t.trim().is_empty())?;
            Some(WindowInfo {
                title,
                app: w.app_name().unwrap_or_default(),
                pid: w.pid().map(i64::from).unwrap_or(0),
                ..Default::default()
            })
        })
        .collect()
}

fn capture_primary_monitor() -> anyhow::Result<DynamicImage> {
    let monitors = xcap::Monitor::all()?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| anyhow::anyhow!("no monitors found"))?;
    Ok(DynamicImage::ImageRgba8(monitor.capture_image()?))
}

fn new_enigo(prompt_for_permission: bool) -> Result<enigo::Enigo, enigo::NewConError> {
    let settings = enigo::Settings { open_prompt_to_get_permissions: prompt_for_permission, ..Default::default() };
    enigo::Enigo::new(&settings)
}

/// Ways this machine can synthesize the paste shortcut, best first.
#[allow(dead_code)] // only `fane doctor` on Linux lists them
pub fn paste_methods() -> Vec<&'static str> {
    vec!["enigo"]
}

/// Press the paste shortcut in the focused window (Ctrl+V; Ctrl+Shift+V in Linux terminals;
/// Cmd+V on macOS). Blocking. Returns the method used, or None if nothing worked.
pub fn send_paste(_terminal: bool) -> Option<&'static str> {
    use enigo::{Direction, Key, Keyboard};

    // macOS: the physical V key (kVK_ANSI_V). Looking the key up by character goes through
    // the Text Input Sources API, which must run on the main thread on recent macOS.
    #[cfg(target_os = "macos")]
    let (modifier, v) = (Key::Meta, Key::Other(9));
    #[cfg(target_os = "windows")]
    let (modifier, v) = (Key::Control, Key::Unicode('v'));

    let mut enigo = match new_enigo(true) {
        Ok(e) => e,
        Err(e) => {
            warn!("paste via enigo failed: {e}");
            return None;
        }
    };
    let result = enigo
        .key(modifier, Direction::Press)
        .and_then(|()| enigo.key(v, Direction::Click))
        .and_then(|()| enigo.key(modifier, Direction::Release));
    match result {
        Ok(()) => Some("enigo"),
        Err(e) => {
            warn!("paste via enigo failed: {e}");
            let _ = enigo.key(modifier, Direction::Release);
            None
        }
    }
}

/// Start Fane at login (or stop). Returns a human-readable description of what was done.
pub fn set_autostart(on: bool) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy();
    let mut builder = auto_launch::AutoLaunchBuilder::new();
    builder.set_app_name("Fane").set_app_path(&exe).set_args(&["run"]);
    #[cfg(target_os = "macos")]
    builder.set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent);
    #[cfg(target_os = "windows")]
    builder.set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);
    let launcher = builder.build()?;

    #[cfg(target_os = "macos")]
    let place = dirs::home_dir()
        .map(|h| h.join("Library/LaunchAgents/Fane.plist").display().to_string())
        .unwrap_or_else(|| "~/Library/LaunchAgents/Fane.plist".into());
    #[cfg(target_os = "windows")]
    let place = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Fane".to_string();

    if on {
        launcher.enable()?;
        Ok(format!("Autostart enabled ({place})."))
    } else {
        launcher.disable()?;
        Ok("Autostart disabled.".into())
    }
}

const LINUX_ONLY: &str = "Only needed on Linux with GNOME.";

/// Linux: install the bundled GNOME Shell helper extension. Returns a message for the user.
pub fn install_extension() -> anyhow::Result<String> {
    Ok(format!(
        "{LINUX_ONLY} On this system Fane reads the focused window and the screen with native APIs, \
         so there is no extension to install."
    ))
}

/// Linux: bind a GNOME custom shortcut to `<exe> toggle`. Returns a message for the user.
pub fn setup_shortcut(binding: &str) -> anyhow::Result<String> {
    Ok(format!(
        "{LINUX_ONLY} On this system Fane registers its hotkey ({binding}) itself while it is running; \
         change it with `binding` under `[hotkey]` in the config file."
    ))
}

/// Extra lines for `fane doctor` (checks specific to this OS), as (ok, text).
pub fn doctor_checks() -> Vec<(bool, String)> {
    let mut out = vec![];
    #[cfg(target_os = "macos")]
    out.push((true, "session: macOS".to_string()));
    #[cfg(target_os = "windows")]
    out.push((true, "session: Windows".to_string()));

    match new_enigo(false) {
        Ok(_) => out.push((true, "paste methods: enigo".into())),
        #[cfg(target_os = "macos")]
        Err(enigo::NewConError::NoPermission) => out.push((
            false,
            "paste methods: enigo needs the Accessibility permission (System Settings → Privacy & Security → \
             Accessibility) — will copy to clipboard only"
                .into(),
        )),
        Err(e) => out.push((false, format!("paste methods: none ({e}) — will copy to clipboard only"))),
    }
    #[cfg(target_os = "macos")]
    out.push((
        true,
        "screenshots need the Screen Recording permission (System Settings → Privacy & Security → \
         Screen Recording); test with `fane doctor --screenshot`"
            .into(),
    ));
    out
}
