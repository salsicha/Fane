//! Linux: the `fane-helper` GNOME Shell extension over D-Bus, with fallbacks
//! (xprop / wmctrl for windows; grim and the xdg-desktop-portal for screenshots).
//!
//! On GNOME Wayland, ordinary apps can't see which window is focused, take silent
//! screenshots, or place their own windows. The extension (gnome-extension/) runs
//! inside the shell and exposes exactly those things.

mod integration;
mod keys;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use image::DynamicImage;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::config::RUNTIME_DIR;
use crate::snapshot::WindowInfo;

pub use integration::{install_extension, set_autostart, setup_shortcut};
pub use keys::{paste_methods, send_paste};

const BUS_NAME: &str = "io.github.salsicha.Fane";
const OBJ_PATH: &str = "/io/github/salsicha/Fane";
const IFACE: &str = "io.github.salsicha.Fane";

/// How long to wait for a helper call (Screenshot gets longer: it encodes a PNG).
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const SHOT_TIMEOUT: Duration = Duration::from_secs(10);
/// The portal may show a permission dialog; don't wait for it forever.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(30);
const TOOL_TIMEOUT: Duration = Duration::from_secs(10);

/// Desktop context access. Linux: the GNOME helper extension over D-Bus, then fallbacks
/// (xprop / wmctrl, grim, xdg-desktop-portal screenshots).
pub struct Desktop {
    conn: Option<zbus::Connection>,
    helper: bool,
    /// Where screenshots are written (the helper extension only writes under here).
    tmp: PathBuf,
    backend: Mutex<Option<String>>,
    portal_failed: AtomicBool,
}

impl Desktop {
    pub async fn connect() -> Self {
        let conn = match timeout(CALL_TIMEOUT, zbus::Connection::session()).await {
            Ok(Ok(c)) => Some(c),
            Ok(Err(e)) => {
                info!("no D-Bus session bus ({e})");
                None
            }
            Err(_) => {
                info!("timed out connecting to the D-Bus session bus");
                None
            }
        };
        let tmp = RUNTIME_DIR.join("fane");
        if let Err(e) = make_private_dir(&tmp) {
            warn!("could not create {}: {e}", tmp.display());
        }
        let mut desktop =
            Desktop { conn, helper: false, tmp, backend: Mutex::new(None), portal_failed: AtomicBool::new(false) };
        if desktop.conn.is_some() {
            match desktop.raw_call::<_, String>("FocusedWindow", &(), CALL_TIMEOUT).await {
                Ok(_) => desktop.helper = true,
                Err(e) => info!("GNOME helper extension unavailable ({e})"),
            }
        }
        desktop
    }

    /// Linux: whether the GNOME helper extension answered. Always false elsewhere.
    pub fn helper_available(&self) -> bool {
        self.helper
    }

    /// Name of the screenshot backend that last worked ("gnome-helper", "portal", "xcap", ...).
    pub fn screen_backend(&self) -> Option<String> {
        self.backend.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub async fn focused_window(&self) -> WindowInfo {
        if let Some(raw) = self.call::<_, String>("FocusedWindow", &(), CALL_TIMEOUT).await {
            match serde_json::from_str::<WindowInfo>(&raw) {
                // The helper answers `{}` when nothing has focus; fall through like Python did.
                Ok(w) if w != WindowInfo::default() => return w,
                Ok(_) => {}
                Err(e) => warn!("helper FocusedWindow returned bad JSON: {e}"),
            }
        }
        // Only sees X11/XWayland windows, but better than nothing.
        if std::env::var_os("DISPLAY").is_some()
            && which::which("xprop").is_ok()
            && let Some(w) = xprop_focused_window().await
        {
            return w;
        }
        WindowInfo::default()
    }

    pub async fn list_windows(&self) -> Vec<WindowInfo> {
        if self.helper {
            let Some(raw) = self.call::<_, String>("ListWindows", &(), CALL_TIMEOUT).await else {
                return vec![];
            };
            return match serde_json::from_str::<Vec<Option<WindowInfo>>>(&raw) {
                Ok(wins) => wins.into_iter().flatten().collect(),
                Err(e) => {
                    warn!("helper ListWindows returned bad JSON: {e}");
                    vec![]
                }
            };
        }
        if which::which("wmctrl").is_ok()
            && let Some(out) = run_tool("wmctrl", &["-lpx"]).await
        {
            return parse_wmctrl(&out);
        }
        vec![]
    }

    pub async fn screenshot(&self) -> Option<DynamicImage> {
        for backend in self.backends() {
            let img = match backend {
                "gnome-helper" => self.shot_helper().await,
                "x11" => self.shot_import().await,
                "grim" => self.shot_grim().await,
                "portal" => self.shot_portal().await,
                _ => None,
            };
            if let Some(img) = img {
                let mut last = self.backend.lock().unwrap_or_else(|e| e.into_inner());
                if last.as_deref() != Some(backend) {
                    info!("screenshot backend: {backend}");
                    *last = Some(backend.to_string());
                }
                return Some(img);
            }
        }
        None
    }

    /// Linux/Wayland only: ask the helper extension to move (and keep above) a window by title.
    pub async fn place_window(&self, title: &str, x: i32, y: i32, above: bool) -> bool {
        self.call::<_, bool>("PlaceWindow", &(title, x, y, above), CALL_TIMEOUT).await.unwrap_or(false)
    }

    // ---- helper D-Bus calls ----

    async fn raw_call<B, R>(&self, method: &str, body: &B, limit: Duration) -> anyhow::Result<R>
    where
        B: serde::Serialize + zbus::DynamicType,
        R: for<'d> zbus::wire::DynamicDeserialize<'d>,
    {
        let conn = self.conn.as_ref().ok_or_else(|| anyhow::anyhow!("no session bus"))?;
        let reply = timeout(limit, conn.call_method(Some(BUS_NAME), OBJ_PATH, Some(IFACE), method, body))
            .await
            .map_err(|_| anyhow::anyhow!("timed out"))??;
        Ok(reply.body().deserialize::<R>()?)
    }

    async fn call<B, R>(&self, method: &str, body: &B, limit: Duration) -> Option<R>
    where
        B: serde::Serialize + zbus::DynamicType,
        R: for<'d> zbus::wire::DynamicDeserialize<'d>,
    {
        if !self.helper {
            return None;
        }
        match self.raw_call(method, body, limit).await {
            Ok(r) => Some(r),
            Err(e) => {
                warn!("helper {method} failed: {e}");
                None
            }
        }
    }

    // ---- screenshot backends, in order of quietness ----

    fn backends(&self) -> Vec<&'static str> {
        let mut out = vec![];
        if self.helper {
            out.push("gnome-helper");
        }
        if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("x11") && which::which("import").is_ok() {
            out.push("x11");
        }
        if which::which("grim").is_ok() && !is_gnome() {
            out.push("grim");
        }
        if self.conn.is_some() && !self.portal_failed.load(Ordering::Relaxed) {
            out.push("portal");
        }
        out
    }

    async fn shot_helper(&self) -> Option<DynamicImage> {
        let path = self.tmp.join("shot.png");
        let path_str = path.to_str()?;
        if self.call::<_, bool>("Screenshot", &(path_str,), SHOT_TIMEOUT).await? {
            load_image(path).await
        } else {
            None
        }
    }

    /// X11 sessions: ImageMagick's `import -window root`.
    async fn shot_import(&self) -> Option<DynamicImage> {
        let path = self.tmp.join("shot.png");
        let target = format!("png:{}", path.display());
        run_tool("import", &["-window", "root", &target]).await?;
        load_image(path).await
    }

    /// wlroots compositors (sway, Hyprland, ...).
    async fn shot_grim(&self) -> Option<DynamicImage> {
        let path = self.tmp.join("shot.png");
        run_tool("grim", &[path.to_str()?]).await?;
        load_image(path).await
    }

    /// xdg-desktop-portal Screenshot (may ask for permission the first time). Any failure,
    /// denial or timeout disables it for the rest of the session so we don't nag.
    async fn shot_portal(&self) -> Option<DynamicImage> {
        use ashpd::desktop::screenshot::Screenshot;

        let conn = self.conn.clone()?;
        let request = Screenshot::request().interactive(false).modal(false).connection(Some(conn)).send();
        let result = match timeout(PORTAL_TIMEOUT, request).await {
            Ok(Ok(req)) => req.response(),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                warn!("portal screenshot timed out; not trying it again this session");
                self.portal_failed.store(true, Ordering::Relaxed);
                return None;
            }
        };
        let shot = match result {
            Ok(shot) => shot,
            Err(e) => {
                warn!("portal screenshot failed or was denied ({e}); not trying it again this session");
                self.portal_failed.store(true, Ordering::Relaxed);
                return None;
            }
        };
        let path = match url::Url::parse(shot.uri().as_str()).ok().and_then(|u| u.to_file_path().ok()) {
            Some(p) => p,
            None => {
                warn!("portal returned a non-file URI: {}", shot.uri().as_str());
                return None;
            }
        };
        let img = load_image(path.clone()).await;
        // The portal saves a file for every request; remove the one it just made for us.
        let fresh = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < Duration::from_secs(15));
        if fresh && let Err(e) = std::fs::remove_file(&path) {
            debug!("could not remove portal screenshot {}: {e}", path.display());
        }
        img
    }
}

/// Extra lines for `fane doctor` (checks specific to this OS), as (ok, text).
pub fn doctor_checks() -> Vec<(bool, String)> {
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "?".into());
    let mut out = vec![(true, format!("session: {} / {}", env("XDG_SESSION_TYPE"), env("XDG_CURRENT_DESKTOP")))];

    let helper = probe_helper_blocking();
    let text = if helper {
        "GNOME helper extension".to_string()
    } else if is_gnome() {
        "GNOME helper extension (run `fane install-extension`)".to_string()
    } else {
        "GNOME helper extension (not on GNOME — using fallbacks)".to_string()
    };
    out.push((helper, text));

    let methods = paste_methods();
    let list = if methods.is_empty() { "none — will copy to clipboard only".to_string() } else { methods.join(", ") };
    out.push((!methods.is_empty(), format!("paste methods: {list}")));
    out
}

/// `doctor_checks` is synchronous; probe the helper on a throwaway runtime in its own
/// thread so this works whether or not the caller is inside a tokio runtime.
fn probe_helper_blocking() -> bool {
    std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| rt.block_on(async { Desktop::connect().await.helper_available() }))
            .unwrap_or(false)
    })
    .join()
    .unwrap_or(false)
}

fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("gnome")))
        .unwrap_or(false)
}

fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

async fn load_image(path: PathBuf) -> Option<DynamicImage> {
    match tokio::task::spawn_blocking(move || image::open(&path).map_err(|e| (path, e))).await {
        Ok(Ok(img)) => Some(img),
        Ok(Err((path, e))) => {
            debug!("could not load {}: {e}", path.display());
            None
        }
        Err(e) => {
            debug!("image loading task failed: {e}");
            None
        }
    }
}

/// Run a tool with a timeout; its stdout if it exited successfully.
async fn run_tool(program: &str, args: &[&str]) -> Option<String> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match timeout(TOOL_TIMEOUT, child).await {
        Ok(Ok(out)) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(Ok(out)) => {
            debug!("{program} exited with {}", out.status);
            None
        }
        Ok(Err(e)) => {
            debug!("{program} failed: {e}");
            None
        }
        Err(_) => {
            debug!("{program} timed out");
            None
        }
    }
}

async fn xprop_focused_window() -> Option<WindowInfo> {
    let root = run_tool("xprop", &["-root", "_NET_ACTIVE_WINDOW"]).await?;
    let id = parse_active_window_id(&root)?;
    let props = run_tool("xprop", &["-id", &id, "_NET_WM_NAME", "WM_CLASS", "_NET_WM_PID"]).await?;
    Some(parse_xprop_window(&props))
}

/// The id in `_NET_ACTIVE_WINDOW(WINDOW): window id # 0x3a00007`, unless it is 0x0.
fn parse_active_window_id(root: &str) -> Option<String> {
    let re = regex::Regex::new(r"(0x[0-9a-f]+)").unwrap();
    let id = re.captures(root)?.get(1)?.as_str();
    (id != "0x0").then(|| id.to_string())
}

fn parse_xprop_window(props: &str) -> WindowInfo {
    let title_re = regex::Regex::new(r#"_NET_WM_NAME\(\w+\) = "(.*)""#).unwrap();
    let pid_re = regex::Regex::new(r"_NET_WM_PID\(\w+\) = (\d+)").unwrap();
    let quoted = regex::Regex::new(r#""([^"]*)""#).unwrap();
    let title = title_re.captures(props).map(|c| c[1].to_string()).unwrap_or_default();
    // WM_CLASS(STRING) = "instance", "Class" — the last one is the class.
    let class_line = props.split_once("WM_CLASS").map_or(props, |(_, rest)| rest);
    let class_line = class_line.split('\n').next().unwrap_or("");
    let app = quoted.captures_iter(class_line).last().map(|c| c[1].to_string()).unwrap_or_default();
    let pid = pid_re.captures(props).and_then(|c| c[1].parse().ok()).unwrap_or(0);
    WindowInfo { title, app, pid, ..Default::default() }
}

/// `wmctrl -lpx` lines: `<id> <desktop> <pid> <wm_class> <host> <title...>`.
fn parse_wmctrl(out: &str) -> Vec<WindowInfo> {
    out.lines()
        .filter_map(|line| {
            let parts = split_whitespace_n(line, 4);
            if parts.len() != 5 {
                return None;
            }
            let title = parts[4].split_once(' ').map_or(parts[4], |(_, t)| t);
            Some(WindowInfo {
                title: title.to_string(),
                app: parts[3].to_string(),
                pid: parts[2].parse().unwrap_or(0),
                ..Default::default()
            })
        })
        .collect()
}

/// Python's `str.split(None, maxsplit)`.
fn split_whitespace_n(s: &str, maxsplit: usize) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = s.trim_start();
    while !rest.is_empty() {
        if out.len() == maxsplit {
            out.push(rest);
            break;
        }
        match rest.find(char::is_whitespace) {
            Some(i) => {
                out.push(&rest[..i]);
                rest = rest[i..].trim_start();
            }
            None => {
                out.push(rest);
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xprop_parsing() {
        assert_eq!(parse_active_window_id("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x3a00007\n").as_deref(),
                   Some("0x3a00007"));
        assert_eq!(parse_active_window_id("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x0\n"), None);
        let props = "_NET_WM_NAME(UTF8_STRING) = \"notes.txt - gedit\"\n\
                     WM_CLASS(STRING) = \"gedit\", \"Gedit\"\n\
                     _NET_WM_PID(CARDINAL) = 4242\n";
        let w = parse_xprop_window(props);
        assert_eq!((w.title.as_str(), w.app.as_str(), w.pid), ("notes.txt - gedit", "Gedit", 4242));
        let w = parse_xprop_window("_NET_WM_NAME:  not found.\nWM_CLASS:  not found.\n");
        assert_eq!(w, WindowInfo::default());
    }

    #[test]
    fn wmctrl_parsing() {
        let out = "0x03a00007  0 4242   gedit.Gedit           myhost notes.txt - gedit\n\
                   0x04000003 -1 99     nautilus.Nautilus     myhost Home\n\
                   garbage\n";
        let wins = parse_wmctrl(out);
        assert_eq!(wins.len(), 2);
        assert_eq!((wins[0].title.as_str(), wins[0].app.as_str(), wins[0].pid), ("notes.txt - gedit", "gedit.Gedit", 4242));
        assert_eq!((wins[1].title.as_str(), wins[1].pid), ("Home", 99));
    }

    #[test]
    fn python_like_split() {
        assert_eq!(split_whitespace_n("  a  b   c d ", 2), vec!["a", "b", "c d "]);
        assert_eq!(split_whitespace_n("a b", 4), vec!["a", "b"]);
        assert!(split_whitespace_n("   ", 4).is_empty());
    }

    /// Real-desktop smoke test: `cargo test smoke -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn smoke() {
        let _ = tracing_subscriber::fmt().with_env_filter("fane=debug").with_test_writer().try_init();
        let d = Desktop::connect().await;
        println!("helper available: {}", d.helper_available());
        println!("focused window: {:?}", d.focused_window().await);
        println!("windows: {}", d.list_windows().await.len());
        let shot = timeout(Duration::from_secs(45), d.screenshot()).await;
        match shot {
            Ok(Some(img)) => println!("screenshot: {}x{} via {:?}", img.width(), img.height(), d.screen_backend()),
            Ok(None) => println!("screenshot: no backend worked"),
            Err(_) => println!("screenshot: timed out"),
        }
        println!("paste methods: {:?}", paste_methods());
        for (ok, line) in doctor_checks() {
            println!("{} {line}", if ok { "✓" } else { "✗" });
        }
    }
}
