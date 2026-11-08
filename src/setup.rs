//! First-run setup: install Ollama if it's missing (with Ollama's official installer for this
//! OS), start it, and download Fane's default model (Gemma 4 26B) if it isn't there yet.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::ModelCfg;
use crate::llm::find_installed;

#[cfg(target_os = "linux")]
const INSTALL_SH: &str = "https://ollama.com/install.sh";
#[cfg(target_os = "windows")]
const WINDOWS_SETUP: &str = "https://ollama.com/download/OllamaSetup.exe";
#[cfg(target_os = "macos")]
const MACOS_ZIP: &str = "https://ollama.com/download/Ollama-darwin.zip";

/// What setup still has to do on this machine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub host: String,
    /// Ollama isn't installed here.
    pub install: bool,
    /// Ollama is installed (or about to be) but not answering.
    pub start: bool,
    /// The model to download, if it's missing (or might be, when Ollama isn't running yet).
    pub model: Option<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        !self.install && !self.start && self.model.is_none()
    }

    /// One line for a confirmation prompt or card.
    pub fn describe(&self) -> String {
        let mut parts = vec![];
        if self.install {
            parts.push("install Ollama".to_string());
        } else if self.start {
            parts.push("start Ollama".to_string());
        }
        if let Some(m) = &self.model {
            parts.push(format!("download {m}{}", size_hint(m)));
        }
        let mut s = parts.join(" and ");
        if let Some(first) = s.get(..1) {
            s = first.to_uppercase() + &s[1..];
        }
        s
    }
}

fn size_hint(model: &str) -> &'static str {
    if model.starts_with("gemma4:26b") { " (about 18 GB)" } else { "" }
}

pub fn is_local(host: &str) -> bool {
    url::Url::parse(host)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "localhost" || h == "[::1]" || h.starts_with("127.")))
        .unwrap_or(false)
}

fn client(read_timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(read_timeout)
        .build()
        .unwrap_or_else(|_| crate::llm::http_client(read_timeout))
}

async fn version(host: &str) -> Option<String> {
    let r = client(Duration::from_secs(3)).get(format!("{host}/api/version")).send().await.ok()?;
    let v: Value = r.json().await.ok()?;
    v["version"].as_str().map(String::from)
}

async fn models(host: &str) -> Result<Vec<String>, String> {
    let r = client(Duration::from_secs(10))
        .get(format!("{host}/api/tags"))
        .send()
        .await
        .map_err(|e| format!("Ollama at {host} isn't answering: {e}"))?;
    let v: Value = r.json().await.map_err(|e| format!("bad reply from Ollama: {e}"))?;
    Ok(v["models"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect())
        .unwrap_or_default())
}

/// Where Ollama is installed on this machine, if it is.
pub fn ollama_binary() -> Option<PathBuf> {
    if let Ok(p) = which::which("ollama") {
        return Some(p);
    }
    let home = dirs::home_dir().unwrap_or_default();
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/Applications/Ollama.app/Contents/Resources/ollama"),
            home.join("Applications/Ollama.app/Contents/Resources/ollama"),
        ]
    } else if cfg!(target_os = "windows") {
        let local = dirs::data_local_dir().unwrap_or_else(|| home.join("AppData/Local"));
        vec![local.join("Programs/Ollama/ollama.exe")]
    } else {
        vec![PathBuf::from("/usr/local/bin/ollama"), PathBuf::from("/usr/bin/ollama")]
    };
    candidates.into_iter().find(|p| p.is_file())
}

/// What's missing for `cfg` (Ollama server and the configured model). Errors only when the
/// configured server is another computer that isn't answering: Fane can't install there.
pub async fn plan(cfg: &ModelCfg) -> Result<Plan, String> {
    let host = cfg.host.trim_end_matches('/').to_string();
    let mut plan = Plan { host: host.clone(), ..Default::default() };
    if version(&host).await.is_some() {
        let installed = models(&host).await?;
        if find_installed(&cfg.name, &installed).is_none() {
            plan.model = Some(cfg.name.clone());
        }
        return Ok(plan);
    }
    if !is_local(&host) {
        return Err(format!(
            "the Ollama server at {host} isn't answering; start it there, or pick another in Settings"
        ));
    }
    plan.install = ollama_binary().is_none();
    plan.start = true;
    plan.model = Some(cfg.name.clone());
    Ok(plan)
}

pub type Progress<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// Carry out `plan`. `gui`: no terminal to ask for a password in (Linux uses pkexec).
pub async fn run(plan: &Plan, gui: bool, progress: Progress<'_>) -> Result<(), String> {
    if plan.install {
        install_ollama(gui, progress).await?;
    }
    if plan.start || plan.install {
        start_ollama(&plan.host, progress).await?;
    }
    if let Some(model) = &plan.model {
        // When Ollama wasn't running we didn't know what it has; check now.
        let installed = models(&plan.host).await?;
        if find_installed(model, &installed).is_none() {
            pull(&plan.host, model, progress).await?;
        }
    }
    Ok(())
}

async fn download(url: &str, dest: &Path, what: &str, progress: Progress<'_>) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut resp = client(Duration::from_secs(120))
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("couldn't download {what}: {e}"))?;
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(dest).await.map_err(|e| format!("can't write {}: {e}", dest.display()))?;
    let (mut done, mut last) = (0u64, Instant::now());
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("download of {what} failed: {e}"))? {
        file.write_all(&chunk).await.map_err(|e| format!("can't write {}: {e}", dest.display()))?;
        done += chunk.len() as u64;
        if last.elapsed() > Duration::from_millis(500) {
            last = Instant::now();
            progress(&format!("Downloading {what}… {}", amount(done, total)));
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn amount(done: u64, total: Option<u64>) -> String {
    match total {
        Some(t) if t > 0 => format!("{} of {} ({}%)", gb(done), gb(t), done * 100 / t),
        _ => gb(done),
    }
}

async fn run_installer(mut cmd: tokio::process::Command, what: &str) -> Result<(), String> {
    let status = cmd.status().await.map_err(|e| format!("couldn't start {what}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{what} failed ({status})")) }
}

async fn install_ollama(gui: bool, progress: Progress<'_>) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("fane-setup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = install_in(&dir, gui, progress).await;
    let _ = std::fs::remove_dir_all(&dir);
    result?;
    progress("Ollama is installed.");
    Ok(())
}

#[cfg(target_os = "linux")]
async fn install_in(dir: &Path, gui: bool, progress: Progress<'_>) -> Result<(), String> {
    // Ollama's official script: installs to /usr/local and sets up a systemd service.
    let missing: Vec<&str> = ["curl", "zstd"].into_iter().filter(|t| which::which(t).is_err()).collect();
    if !missing.is_empty() {
        return Err(format!(
            "Ollama's installer needs {}; install with: sudo apt install {}",
            missing.join(" and "),
            missing.join(" ")
        ));
    }
    let script = dir.join("install.sh");
    download(INSTALL_SH, &script, "the Ollama installer", progress).await?;
    progress("Installing Ollama (asks for your password)…");
    let mut cmd = if gui {
        if which::which("pkexec").is_err() {
            return Err("installing Ollama needs administrator rights; run `fane setup` in a terminal".into());
        }
        let mut c = tokio::process::Command::new("pkexec");
        c.arg("sh").arg(&script);
        c
    } else {
        // The script asks for sudo itself when it needs it.
        let mut c = tokio::process::Command::new("sh");
        c.arg(&script);
        c
    };
    cmd.stdin(if gui { std::process::Stdio::null() } else { std::process::Stdio::inherit() });
    run_installer(cmd, "the Ollama installer").await
}

#[cfg(target_os = "macos")]
async fn install_in(dir: &Path, _gui: bool, progress: Progress<'_>) -> Result<(), String> {
    let zip = dir.join("Ollama-darwin.zip");
    download(MACOS_ZIP, &zip, "Ollama", progress).await?;
    progress("Installing Ollama…");
    let mut unzip = tokio::process::Command::new("ditto");
    unzip.args(["-x", "-k"]).arg(&zip).arg(dir);
    run_installer(unzip, "unpacking Ollama").await?;
    let system = PathBuf::from("/Applications");
    let target = if is_writable(&system) {
        system
    } else {
        let user = dirs::home_dir().unwrap_or_default().join("Applications");
        std::fs::create_dir_all(&user).map_err(|e| e.to_string())?;
        user
    };
    let dest = target.join("Ollama.app");
    let _ = std::fs::remove_dir_all(&dest);
    let mut mv = tokio::process::Command::new("mv");
    mv.arg(dir.join("Ollama.app")).arg(&dest);
    run_installer(mv, "moving Ollama to Applications").await
}

#[cfg(target_os = "macos")]
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".fane-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(target_os = "windows")]
async fn install_in(dir: &Path, _gui: bool, progress: Progress<'_>) -> Result<(), String> {
    // Per-user Inno Setup installer; no administrator rights needed.
    let setup = dir.join("OllamaSetup.exe");
    download(WINDOWS_SETUP, &setup, "Ollama", progress).await?;
    progress("Installing Ollama…");
    let mut cmd = tokio::process::Command::new(&setup);
    cmd.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
    run_installer(cmd, "the Ollama installer").await
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
async fn install_in(_dir: &Path, _gui: bool, _progress: Progress<'_>) -> Result<(), String> {
    Err("install Ollama from https://ollama.com/download".into())
}

/// Start the local server if it isn't answering, and wait (up to a minute) until it is.
async fn start_ollama(host: &str, progress: Progress<'_>) -> Result<(), String> {
    if version(host).await.is_some() {
        return Ok(());
    }
    progress("Starting Ollama…");
    let bin = ollama_binary().ok_or("Ollama isn't installed")?;
    spawn_server(&bin);
    if wait_for(host, Duration::from_secs(20)).await {
        return Ok(());
    }
    // The desktop app can sit on a first-run dialog before it starts its server; run the
    // server directly instead.
    serve(&bin);
    if wait_for(host, Duration::from_secs(40)).await {
        return Ok(());
    }
    Err(format!("Ollama didn't start answering at {host}"))
}

async fn wait_for(host: &str, limit: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if version(host).await.is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

fn spawn_server(bin: &Path) {
    use std::process::Command;
    // The desktop apps run the server themselves (and keep it running at login).
    if cfg!(target_os = "macos")
        && let Some(app) = bin.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))
    {
        let _ = Command::new("open").arg("-a").arg(app).spawn();
        return;
    }
    let tray = bin.with_file_name("ollama app.exe");
    if cfg!(target_os = "windows") && tray.is_file() {
        let _ = Command::new(tray).spawn();
        return;
    }
    // Linux: the installer's systemd service normally runs it; otherwise run it ourselves.
    if cfg!(target_os = "linux")
        && Command::new("systemctl").args(["start", "ollama"]).status().is_ok_and(|s| s.success())
    {
        return;
    }
    serve(bin);
}

/// Run `ollama serve` in the background, detached from Fane.
fn serve(bin: &Path) {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.arg("serve").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.spawn();
}

/// Download `model` through Ollama's API, reporting progress.
pub async fn pull(host: &str, model: &str, progress: Progress<'_>) -> Result<(), String> {
    progress(&format!("Downloading {model}…"));
    let mut resp = client(Duration::from_secs(300))
        .post(format!("{host}/api/pull"))
        .json(&json!({"model": model, "stream": true}))
        .send()
        .await
        .map_err(|e| format!("couldn't start downloading {model}: {e}"))?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Ollama couldn't download {model}: {}", crate::llm::truncate(&body, 200)));
    }
    let mut tracker = PullProgress::default();
    let mut buf = Vec::new();
    let mut last = Instant::now() - Duration::from_secs(1);
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("download of {model} failed: {e}"))? {
        buf.extend_from_slice(&chunk);
        while let Some(i) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            let Ok(event) = serde_json::from_slice::<Value>(&line) else { continue };
            if let Some(e) = event["error"].as_str() {
                return Err(format!("Ollama couldn't download {model}: {e}"));
            }
            tracker.update(&event);
            if tracker.success {
                progress(&format!("{model} is ready."));
                return Ok(());
            }
            if last.elapsed() > Duration::from_millis(500) {
                last = Instant::now();
                progress(&format!("Downloading {model}… {}", tracker.text()));
            }
        }
    }
    if tracker.success { Ok(()) } else { Err(format!("the download of {model} stopped before it finished")) }
}

/// Sums Ollama's per-layer pull events into one figure.
#[derive(Debug, Default)]
pub struct PullProgress {
    layers: std::collections::BTreeMap<String, (u64, u64)>,
    status: String,
    pub success: bool,
}

impl PullProgress {
    pub fn update(&mut self, event: &Value) {
        let status = event["status"].as_str().unwrap_or("");
        self.success |= status == "success";
        self.status = status.to_string();
        if let (Some(digest), Some(total)) = (event["digest"].as_str(), event["total"].as_u64()) {
            let done = event["completed"].as_u64().unwrap_or(0);
            self.layers.insert(digest.to_string(), (done.min(total), total));
        }
    }

    pub fn text(&self) -> String {
        let (done, total) = self.layers.values().fold((0, 0), |(d, t), (ld, lt)| (d + ld, t + lt));
        if total > 0 { amount(done, Some(total)) } else { self.status.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_plans() {
        let p = Plan { install: true, start: true, model: Some("gemma4:26b".into()), ..Default::default() };
        assert_eq!(p.describe(), "Install Ollama and download gemma4:26b (about 18 GB)");
        let p = Plan { start: true, ..Default::default() };
        assert_eq!(p.describe(), "Start Ollama");
        assert!(Plan::default().is_empty());
    }

    #[test]
    fn local_hosts() {
        assert!(is_local("http://127.0.0.1:11434") && is_local("http://localhost:11434"));
        assert!(!is_local("http://192.168.1.20:11434"));
    }

    #[test]
    fn pull_progress_sums_layers() {
        let mut p = PullProgress::default();
        p.update(&json!({"status": "pulling manifest"}));
        assert_eq!(p.text(), "pulling manifest");
        p.update(
            &json!({"status": "pulling a", "digest": "a", "total": 8_000_000_000u64, "completed": 2_000_000_000u64}),
        );
        p.update(&json!({"status": "pulling b", "digest": "b", "total": 2_000_000_000u64, "completed": 0}));
        assert_eq!(p.text(), "2.0 GB of 10.0 GB (20%)");
        p.update(&json!({"status": "success"}));
        assert!(p.success);
    }

    /// A fake Ollama that has `models` and streams a short pull.
    async fn fake(models: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /api/version") {
                    r#"{"version":"0.99.0"}"#.to_string()
                } else if req.starts_with("GET /api/tags") {
                    format!(r#"{{"models":[{models}]}}"#)
                } else {
                    concat!(
                        "{\"status\":\"pulling manifest\"}\n",
                        "{\"status\":\"pulling x\",\"digest\":\"x\",\"total\":100,\"completed\":50}\n",
                        "{\"status\":\"success\"}\n"
                    )
                    .to_string()
                };
                let resp =
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn plans_and_pulls_against_a_running_server() {
        let cfg = |host: &str| ModelCfg { host: host.into(), name: "gemma4:26b".into(), ..Default::default() };
        let (have, t1) = fake(r#"{"name":"gemma4:26b"}"#).await;
        assert!(plan(&cfg(&have)).await.unwrap().is_empty());
        let (missing, t2) = fake(r#"{"name":"llama3:8b"}"#).await;
        let p = plan(&cfg(&missing)).await.unwrap();
        assert_eq!(p.model.as_deref(), Some("gemma4:26b"));
        assert!(!p.install && !p.start);
        let seen = std::sync::Mutex::new(vec![]);
        pull(&missing, "gemma4:26b", &|s: &str| seen.lock().unwrap().push(s.to_string())).await.unwrap();
        assert_eq!(seen.lock().unwrap().last().unwrap(), "gemma4:26b is ready.");
        t1.abort();
        t2.abort();
    }

    #[tokio::test]
    async fn remote_server_down_is_an_error() {
        let cfg = ModelCfg { host: "http://192.0.2.1:11434".into(), ..Default::default() };
        assert!(plan(&cfg).await.unwrap_err().contains("isn't answering"));
    }
}
