//! One-click setup of desktop apps that Fane can drive through MCP: install the app if it's
//! missing, add the app's MCP add-on (and turn it on), get the MCP server's runner (uv or
//! Node), and register the server in `[mcp.servers]`. Afterwards Fane opens the app by itself
//! when one of its tools is used.
//!
//! Apps come from the system's own package managers (Flathub without administrator rights, or
//! snap, on Linux; Homebrew on macOS; winget on Windows); add-ons and servers from their
//! projects (pinned where they publish releases); uv from its GitHub releases into
//! `~/.fane/bin`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;

use crate::config::McpServerCfg;
use crate::runtime::download::{self, Progress};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Runner {
    /// A Python MCP server run with `uvx <package>`.
    Uvx,
    /// A Node MCP server run with `npx -y <package>`.
    Npx,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Addon {
    None,
    /// mcp-for-blender's add-on, installed into Blender's user add-ons and enabled.
    Blender,
    /// freecad-mcp's FreeCADMCP workbench, copied into FreeCAD's Mod folder, auto-starting.
    FreeCad,
}

/// A desktop app Fane knows how to set up.
pub struct AppSpec {
    pub id: &'static str,
    pub name: &'static str,
    /// What Fane can do with it.
    pub what: &'static str,
    /// Commands that start it, looked up on PATH.
    pub binaries: &'static [&'static str],
    pub flatpak: Option<&'static str>,
    /// (snap name, needs --classic)
    pub snap: Option<(&'static str, bool)>,
    pub brew_cask: Option<&'static str>,
    pub winget: Option<&'static str>,
    /// macOS app bundle and the executable inside it.
    pub mac_app: Option<(&'static str, &'static str)>,
    /// Windows: (folder under Program Files, glob of the executable inside).
    pub windows: Option<(&'static str, &'static str)>,
    pub runner: Runner,
    pub server_args: &'static [&'static str],
    pub addon: Addon,
    /// The port the app's add-on listens on (0: the server starts what it needs itself).
    pub port: u16,
    pub homepage: &'static str,
}

pub const APPS: [AppSpec; 3] = [
    AppSpec {
        id: "blender",
        name: "Blender",
        what: "make and edit 3D models, scenes, materials and renders",
        binaries: &["blender"],
        flatpak: Some("org.blender.Blender"),
        snap: Some(("blender", true)),
        brew_cask: Some("blender"),
        winget: Some("BlenderFoundation.Blender"),
        mac_app: Some(("Blender.app", "Contents/MacOS/Blender")),
        windows: Some(("Blender Foundation", "Blender*/blender.exe")),
        runner: Runner::Uvx,
        server_args: &["mcp-for-blender@2.2.7"],
        addon: Addon::Blender,
        port: 9876,
        homepage: "https://github.com/ahujasid/mcp-for-blender",
    },
    AppSpec {
        id: "freecad",
        name: "FreeCAD",
        what: "design precise parts and assemblies (CAD), and run FEM analyses",
        binaries: &["freecad", "FreeCAD"],
        flatpak: Some("org.freecad.FreeCAD"),
        snap: Some(("freecad", false)),
        brew_cask: Some("freecad"),
        winget: Some("FreeCAD.FreeCAD"),
        mac_app: Some(("FreeCAD.app", "Contents/MacOS/FreeCAD")),
        windows: Some(("FreeCAD*", "bin/freecad.exe")),
        runner: Runner::Uvx,
        server_args: &["freecad-mcp@0.1.30", "--only-text-feedback"],
        addon: Addon::FreeCad,
        port: 9875,
        homepage: "https://github.com/neka-nat/freecad-mcp",
    },
    AppSpec {
        id: "browser",
        name: "Web browser",
        what: "browse websites, fill in forms and read pages (Playwright, in its own browser profile)",
        binaries: &["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge"],
        flatpak: None,
        snap: None,
        brew_cask: Some("google-chrome"),
        winget: Some("Google.Chrome"),
        mac_app: Some(("Google Chrome.app", "Contents/MacOS/Google Chrome")),
        windows: Some(("Google", "Chrome/Application/chrome.exe")),
        runner: Runner::Npx,
        server_args: &["@playwright/mcp@0.0.85", "--browser", "chrome"],
        addon: Addon::None,
        port: 0,
        homepage: "https://github.com/microsoft/playwright-mcp",
    },
];

pub fn spec(id: &str) -> Option<&'static AppSpec> {
    APPS.iter().find(|a| a.id == id)
}

fn run_quiet(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The command that opens `app` on this computer, if it's installed.
pub fn find(app: &AppSpec) -> Option<Vec<String>> {
    for b in app.binaries {
        if let Ok(p) = which::which(b) {
            return Some(vec![p.display().to_string()]);
        }
    }
    if cfg!(target_os = "linux") {
        if let Some((snap, _)) = app.snap {
            let p = PathBuf::from("/snap/bin").join(snap);
            if p.exists() {
                return Some(vec![p.display().to_string()]);
            }
        }
        if let Some(id) = app.flatpak
            && which::which("flatpak").is_ok()
            && run_quiet("flatpak", &["info", id]).is_some()
        {
            return Some(vec!["flatpak".into(), "run".into(), id.into()]);
        }
    }
    if cfg!(target_os = "macos")
        && let Some((bundle, exe)) = app.mac_app
    {
        let home = dirs::home_dir().unwrap_or_default();
        for root in [PathBuf::from("/Applications"), home.join("Applications")] {
            let p = root.join(bundle).join(exe);
            if p.is_file() {
                return Some(vec![p.display().to_string()]);
            }
        }
    }
    if cfg!(windows)
        && let Some((folder, exe)) = app.windows
    {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            let Ok(root) = std::env::var(var) else { continue };
            let pattern = format!("{root}/{folder}/{exe}").replace('\\', "/");
            if let Some(p) = glob_first(&pattern) {
                return Some(vec![p.display().to_string()]);
            }
        }
    }
    None
}

/// The first existing path matching a pattern with `*` in its directory or file names
/// (newest name first, so "Blender 4.5" beats "Blender 4.2").
fn glob_first(pattern: &str) -> Option<PathBuf> {
    let mut paths = vec![PathBuf::new()];
    for (i, part) in pattern.split('/').enumerate() {
        if part.contains('*') {
            let re = regex::Regex::new(&format!("^{}$", regex::escape(part).replace("\\*", ".*"))).ok()?;
            let mut next = vec![];
            for base in &paths {
                let Ok(rd) = std::fs::read_dir(base) else { continue };
                let mut found: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok())
                    .filter(|e| re.is_match(&e.file_name().to_string_lossy()))
                    .map(|e| e.path())
                    .collect();
                found.sort();
                found.reverse();
                next.extend(found);
            }
            paths = next;
        } else if i == 0 {
            paths = vec![PathBuf::from(if part.is_empty() { "/" } else { part })];
        } else {
            paths = paths.into_iter().map(|p| p.join(part)).collect();
        }
    }
    paths.into_iter().find(|p| p.exists())
}

/// Where Fane keeps helper programs (uv).
fn bin_dir() -> PathBuf {
    crate::runtime::install::home().join("bin")
}

/// `uvx` / `npx`, if available (Fane's own uv counts).
pub fn runner_path(runner: Runner) -> Option<PathBuf> {
    let name = match runner {
        Runner::Uvx => "uvx",
        Runner::Npx => "npx",
    };
    if let Ok(p) = which::which(name) {
        return Some(p);
    }
    let own = bin_dir().join(crate::runtime::install::exe(name));
    if own.is_file() {
        return Some(own);
    }
    // Shells set up nvm or uv's installer in profiles that a desktop session may not read.
    let home = dirs::home_dir().unwrap_or_default();
    let mut candidates = vec![home.join(".local/bin").join(name), home.join(".cargo/bin").join(name)];
    if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        let mut versions: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path().join("bin").join(name)).collect();
        versions.sort();
        candidates.extend(versions.into_iter().rev());
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// What setting `app` up would do here, as phrases for a confirmation card.
pub fn plan(app: &AppSpec) -> Result<Vec<String>, String> {
    let mut steps = vec![];
    if find(app).is_none() {
        steps.push(format!("install {} with {}", app.name, install_method(app)?));
    }
    if runner_path(app.runner).is_none() {
        match app.runner {
            Runner::Uvx => steps.push("download uv (Python tool runner) into ~/.fane/bin".into()),
            Runner::Npx => {
                return Err("this needs Node.js (npx); install it from https://nodejs.org first".into());
            }
        }
    }
    match app.addon {
        Addon::Blender => steps.push("install and turn on the MCP for Blender add-on".into()),
        Addon::FreeCad => steps.push("install the FreeCAD MCP workbench and have it start with FreeCAD".into()),
        Addon::None => {}
    }
    steps.push(format!(
        "add its MCP server ({}) to Fane",
        app.server_args[0].rsplit_once('@').map_or(app.server_args[0], |(n, _)| n)
    ));
    Ok(steps)
}

fn install_method(app: &AppSpec) -> Result<String, String> {
    let have = |p: &str| which::which(p).is_ok();
    if cfg!(target_os = "linux") {
        if app.flatpak.is_some() && have("flatpak") {
            return Ok("Flatpak (from Flathub, just for you)".into());
        }
        if app.snap.is_some() && have("snap") {
            return Ok("snap (asks for your password)".into());
        }
    } else if cfg!(target_os = "macos") {
        if app.brew_cask.is_some() && have("brew") {
            return Ok("Homebrew".into());
        }
    } else if cfg!(windows) && app.winget.is_some() && have("winget") {
        return Ok("winget (accepting the package's license)".into());
    }
    Err(format!("Fane can't install {} here; install it from {} and try again", app.name, app_site(app)))
}

fn app_site(app: &AppSpec) -> &'static str {
    match app.id {
        "blender" => "https://www.blender.org/download/",
        "freecad" => "https://www.freecad.org/downloads.php",
        "browser" => "https://www.google.com/chrome/",
        _ => app.homepage,
    }
}

async fn run(program: &str, args: &[&str], what: &str) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(1800), cmd.output())
        .await
        .map_err(|_| format!("{what} took too long"))?
        .map_err(|e| format!("{what}: couldn't run {program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> =
            err.lines().chain(stdout.lines()).filter(|l| !l.trim().is_empty()).rev().take(3).collect();
        Err(format!("{what} failed: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" / ")))
    }
}

async fn install_app(app: &AppSpec, gui: bool, progress: Progress<'_>) -> Result<(), String> {
    let have = |p: &str| which::which(p).is_ok();
    progress(&format!("Installing {}…", app.name));
    if cfg!(target_os = "linux") {
        if let Some(id) = app.flatpak.filter(|_| have("flatpak")) {
            run(
                "flatpak",
                &[
                    "remote-add",
                    "--user",
                    "--if-not-exists",
                    "flathub",
                    "https://dl.flathub.org/repo/flathub.flatpakrepo",
                ],
                "adding Flathub",
            )
            .await?;
            run("flatpak", &["install", "--user", "-y", "--noninteractive", "flathub", id], "installing from Flathub")
                .await?;
            return Ok(());
        }
        if let Some((name, classic)) = app.snap.filter(|_| have("snap")) {
            let mut args = vec!["snap", "install", name];
            if classic {
                args.push("--classic");
            }
            let elevate = if gui { "pkexec" } else { "sudo" };
            run(elevate, &args, "installing the snap").await?;
            return Ok(());
        }
    } else if cfg!(target_os = "macos") {
        if let Some(cask) = app.brew_cask.filter(|_| have("brew")) {
            run("brew", &["install", "--cask", cask], "installing with Homebrew").await?;
            return Ok(());
        }
    } else if cfg!(windows)
        && let Some(id) = app.winget.filter(|_| have("winget"))
    {
        run(
            "winget",
            &["install", "--id", id, "-e", "--silent", "--accept-package-agreements", "--accept-source-agreements"],
            "installing with winget",
        )
        .await?;
        return Ok(());
    }
    Err(install_method(app).err().unwrap_or_default())
}

/// uv's release for this computer.
fn uv_asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "uv-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "uv-aarch64-unknown-linux-gnu.tar.gz",
        ("macos", "aarch64") => "uv-aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "uv-x86_64-apple-darwin.tar.gz",
        ("windows", "x86_64") => "uv-x86_64-pc-windows-msvc.zip",
        ("windows", "aarch64") => "uv-aarch64-pc-windows-msvc.zip",
        _ => return None,
    })
}

async fn install_uv(progress: Progress<'_>) -> Result<PathBuf, String> {
    let asset = uv_asset().ok_or("no uv build for this computer; install uv from https://docs.astral.sh/uv/")?;
    let tmp = bin_dir().join("uv-download");
    let _ = std::fs::remove_dir_all(&tmp);
    let archive = tmp.join(asset);
    let url = format!("https://github.com/astral-sh/uv/releases/latest/download/{asset}");
    download::fetch(&url, &archive, "uv", progress).await?;
    let (a, t) = (archive.clone(), tmp.join("x"));
    tokio::task::spawn_blocking(move || download::unpack(&a, &t)).await.map_err(|e| e.to_string())??;
    for entry in walkdir::WalkDir::new(tmp.join("x")).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            std::fs::rename(entry.path(), bin_dir().join(entry.file_name())).map_err(|e| e.to_string())?;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let uvx = bin_dir().join(crate::runtime::install::exe("uvx"));
    if uvx.is_file() { Ok(uvx) } else { Err("the uv download has no uvx".into()) }
}

/// Run a line of Python inside the app (in the background, no window) and return what it
/// printed after `marker`.
async fn app_python(launch: &[String], app: &AppSpec, code: &str, marker: &str) -> Result<String, String> {
    let mut args: Vec<&str> = launch[1..].iter().map(String::as_str).collect();
    match app.addon {
        Addon::Blender => args.extend(["--background", "--python-expr", code]),
        _ => args.extend(["-c", code]),
    }
    let out = run(&launch[0], &args, &format!("asking {}", app.name)).await?;
    out.lines()
        .find_map(|l| l.trim().strip_prefix(marker).map(|s| s.trim().to_string()))
        .ok_or_else(|| format!("{} didn't answer", app.name))
}

async fn install_addon(app: &AppSpec, launch: &[String], uvx: &Path, progress: Progress<'_>) -> Result<(), String> {
    match app.addon {
        Addon::None => Ok(()),
        Addon::Blender => {
            progress("Installing the MCP for Blender add-on…");
            let dir = app_python(
                launch,
                app,
                "import bpy; print('FANE_DIR=' + bpy.utils.user_resource('SCRIPTS', path='addons', create=True))",
                "FANE_DIR=",
            )
            .await?;
            let before = list_dir(Path::new(&dir));
            let uvx_s = uvx.display().to_string();
            run(&uvx_s, &[app.server_args[0], "install-addon", "--addons-dir", &dir], "installing the add-on").await?;
            let after = list_dir(Path::new(&dir));
            let module = after
                .iter()
                .find(|n| !before.contains(n))
                .or_else(|| after.iter().find(|n| n.contains("mcp") || n.contains("blender")))
                .map(|n| n.trim_end_matches(".py").to_string())
                .ok_or("couldn't find the installed add-on")?;
            progress("Turning the add-on on in Blender…");
            let code = format!(
                "import bpy, addon_utils; addon_utils.enable('{module}', default_set=True, persistent=True); \
                 bpy.ops.wm.save_userpref(); print('FANE_OK=' + str(addon_utils.check('{module}')[1]))"
            );
            let ok = app_python(launch, app, &code, "FANE_OK=").await?;
            if ok == "True" { Ok(()) } else { Err("Blender didn't turn the add-on on".into()) }
        }
        Addon::FreeCad => {
            progress("Installing the FreeCAD MCP workbench…");
            let data = app_python(
                launch,
                app,
                "import FreeCAD; print('FANE_DIR=' + FreeCAD.getUserAppDataDir())",
                "FANE_DIR=",
            )
            .await?;
            let data = PathBuf::from(data);
            let tmp = bin_dir().join("freecad-mcp-download");
            let _ = std::fs::remove_dir_all(&tmp);
            let archive = tmp.join("freecad-mcp.tar.gz");
            download::fetch(
                "https://codeload.github.com/neka-nat/freecad-mcp/tar.gz/refs/heads/main",
                &archive,
                "the FreeCAD MCP workbench",
                progress,
            )
            .await?;
            let (a, t) = (archive.clone(), tmp.join("x"));
            tokio::task::spawn_blocking(move || download::unpack(&a, &t)).await.map_err(|e| e.to_string())??;
            let src = walkdir::WalkDir::new(tmp.join("x"))
                .into_iter()
                .filter_map(|e| e.ok())
                .find(|e| {
                    e.file_type().is_dir()
                        && e.file_name() == "FreeCADMCP"
                        && e.path().parent().is_some_and(|p| p.ends_with("addon"))
                })
                .map(|e| e.path().to_path_buf())
                .ok_or("the download has no FreeCADMCP workbench")?;
            let dest = data.join("Mod").join("FreeCADMCP");
            let _ = std::fs::remove_dir_all(&dest);
            copy_dir(&src, &dest)?;
            let _ = std::fs::remove_dir_all(&tmp);
            // Start its RPC server whenever FreeCAD opens.
            let settings = data.join("freecad_mcp_settings.json");
            let mut v: serde_json::Value = std::fs::read_to_string(&settings)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            v["auto_start_rpc"] = serde_json::json!(true);
            std::fs::write(&settings, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

fn list_dir(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    for entry in walkdir::WalkDir::new(from).into_iter().filter_map(|e| e.ok()) {
        let rel = entry.path().strip_prefix(from).unwrap();
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        } else {
            std::fs::copy(entry.path(), &dest).map_err(|e| format!("can't copy to {}: {e}", dest.display()))?;
        }
    }
    Ok(())
}

/// Set `app` up. Returns the MCP server config to register.
pub async fn setup(app: &AppSpec, gui: bool, progress: Progress<'_>) -> Result<McpServerCfg, String> {
    plan(app)?;
    if find(app).is_none() {
        install_app(app, gui, progress).await?;
    }
    let launch = find(app).ok_or_else(|| format!("{} was installed but Fane can't find it", app.name))?;
    let runner = match runner_path(app.runner) {
        Some(p) => p,
        None => install_uv(progress).await?,
    };
    // Fetch the server's packages now, so its first start doesn't time out.
    progress(&format!("Getting the {} MCP server…", app.name));
    let runner_s = runner.display().to_string();
    let mut warm: Vec<&str> = vec![];
    if app.runner == Runner::Npx {
        warm.push("-y");
    }
    warm.push(app.server_args[0]);
    warm.push("--help");
    run(&runner_s, &warm, "getting the MCP server").await?;
    if app.addon != Addon::None {
        let uvx = runner_path(Runner::Uvx).ok_or("uv is missing")?;
        install_addon(app, &launch, &uvx, progress).await?;
    }
    let mut args: Vec<String> = vec![];
    if app.runner == Runner::Npx {
        args.push("-y".into());
    }
    args.extend(app.server_args.iter().map(|s| s.to_string()));
    Ok(McpServerCfg {
        command: runner_s,
        args,
        launch: if app.port > 0 { launch } else { vec![] },
        wait_port: app.port,
        ..Default::default()
    })
}

/// For Settings and `fane apps`.
#[derive(Debug, Clone, Serialize)]
pub struct AppStatus {
    pub id: String,
    pub name: String,
    pub what: String,
    pub installed: bool,
    pub configured: bool,
    pub running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        for app in &APPS {
            assert!(spec(app.id).is_some());
            assert!(!app.server_args.is_empty());
            assert_eq!(app.addon != Addon::None, app.port > 0, "{}: an add-on means a port to wait for", app.id);
        }
        assert!(uv_asset().is_some() || cfg!(not(any(target_os = "linux", target_os = "macos", windows))));
    }

    #[test]
    fn globs_pick_the_newest() {
        let root = std::env::temp_dir().join(format!("fane-glob-{}", std::process::id()));
        for v in ["Blender 4.2", "Blender 4.5"] {
            std::fs::create_dir_all(root.join("Blender Foundation").join(v)).unwrap();
            std::fs::write(root.join("Blender Foundation").join(v).join("blender.exe"), b"").unwrap();
        }
        let pattern = format!("{}/Blender Foundation/Blender*/blender.exe", root.display()).replace('\\', "/");
        let found = glob_first(&pattern).unwrap();
        assert!(found.to_string_lossy().contains("Blender 4.5"), "{}", found.display());
        assert!(glob_first(&format!("{}/Nope*/x.exe", root.display())).is_none());
        std::fs::remove_dir_all(&root).ok();
    }
}
