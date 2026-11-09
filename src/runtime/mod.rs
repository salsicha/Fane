//! Fane's own model server: runs language models with llama.cpp and image/video models with
//! stable-diffusion.cpp on this computer's GPUs, starting each on demand and sharing GPUs
//! between them, and routes every request to where it should run (here, another computer
//! running Fane, or another server) by `[routes]`. It speaks the OpenAI API on
//! `127.0.0.1:<port>`, so Fane's local coding harnesses use it too; with `share` on, other
//! computers can use it with the sharing token.

pub mod arbiter;
pub mod catalog;
pub mod download;
pub mod gpu;
pub mod install;
pub mod router;
pub mod service;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use self::arbiter::{Arbiter, Lease};
use self::catalog::SdSpec;
use self::gpu::{Backend, Gpu};
use crate::config::{Config, Role};

/// Why a target couldn't take a request.
#[derive(Debug)]
pub enum RtError {
    /// Not set up for it (no model files, no runtime build): try the next target.
    NotHere(String),
    /// It should have worked but didn't.
    Failed(String),
}

impl std::fmt::Display for RtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RtError::NotHere(s) | RtError::Failed(s) => f.write_str(s),
        }
    }
}

/// Remote name → (when asked, kind, models).
type RemoteCache = HashMap<String, (Instant, String, Vec<String>)>;

struct Proc {
    child: tokio::process::Child,
    port: u16,
    started: Instant,
}

pub struct Runtime {
    cfg: RwLock<Config>,
    gpus: RwLock<Vec<Gpu>>,
    arbiter: Arc<Arbiter>,
    procs: Mutex<HashMap<String, Proc>>,
    /// Starting a model is one at a time (they compete for disk and GPU anyway).
    starting: tokio::sync::Mutex<()>,
    client: reqwest::Client,
    /// Remote kind and models, cached for a minute: name → (when, kind, models).
    remotes: Mutex<RemoteCache>,
}

impl Runtime {
    pub fn new(cfg: Config) -> Arc<Self> {
        install::set_home(cfg.runtime.home_dir());
        let rt = Arc::new(Self {
            cfg: RwLock::new(cfg),
            gpus: RwLock::new(gpu::detect()),
            arbiter: Arbiter::new(),
            procs: Mutex::new(HashMap::new()),
            starting: tokio::sync::Mutex::new(()),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| crate::llm::http_client(Duration::from_secs(3600))),
            remotes: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&rt);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let Some(rt) = weak.upgrade() else { return };
                rt.reap().await;
            }
        });
        rt
    }

    pub fn cfg(&self) -> Config {
        self.cfg.read().unwrap().clone()
    }

    /// Apply a changed config (GPU choices, routes, remotes). Models already running stay where
    /// they are until they're idle.
    pub fn set_cfg(&self, cfg: Config) {
        install::set_home(cfg.runtime.home_dir());
        *self.cfg.write().unwrap() = cfg;
        self.remotes.lock().unwrap().clear();
    }

    pub fn gpus(&self) -> Vec<Gpu> {
        self.gpus.read().unwrap().clone()
    }

    /// Look for GPUs again (after installing a runtime, which can number them better).
    pub fn redetect(&self) {
        *self.gpus.write().unwrap() = gpu::detect();
    }

    pub fn gpu_for(&self, role: Role) -> Gpu {
        let gpus = self.gpus();
        let cfg = self.cfg();
        gpu::pick(&gpus, cfg.runtime.gpu_for(role)).clone()
    }

    fn budget_mb(&self, gpu: &Gpu) -> u64 {
        let cfg = self.cfg();
        match cfg.runtime.vram_gb.get(&gpu.id) {
            Some(gb) => (gb * 1024.0) as u64,
            None => gpu.vram_mb * 95 / 100,
        }
    }

    /// Which model a role uses here.
    pub fn model_for(&self, role: Role) -> String {
        let cfg = self.cfg();
        match role {
            Role::Chat => cfg.model.name.clone(),
            Role::Code if cfg.model.code.is_empty() => cfg.model.name.clone(),
            Role::Code => cfg.model.code.clone(),
            Role::Image => cfg.runtime.image_model.clone(),
            Role::Video => cfg.runtime.video_model.clone(),
        }
    }

    /// The role a chat request for `model` belongs to.
    pub fn role_of(&self, model: &str) -> Role {
        let cfg = self.cfg();
        let same = |a: &str, b: &str| {
            !a.is_empty()
                && !b.is_empty()
                && (a == b || catalog::llm_spec(a).is_some_and(|s| s.name == b || s.ollama.contains(&b)))
        };
        if same(&cfg.model.code, model) && !same(&cfg.model.name, model) { Role::Code } else { Role::Chat }
    }

    // --- language models ------------------------------------------------------------------

    /// The llama.cpp build to run on `gpu`: its own backend if installed, else Vulkan.
    fn llama_build_for(gpu: &Gpu) -> Option<(Backend, PathBuf)> {
        let wanted = install::llama_backend_for(gpu.backend());
        if let Some(dir) = install::installed_llama(wanted) {
            return Some((wanted, dir));
        }
        if wanted == Backend::Cuda && gpu.vulkan.is_some() {
            return install::installed_llama(Backend::Vulkan).map(|d| (Backend::Vulkan, d));
        }
        None
    }

    /// Whether this computer can run `model` now (files and runtime present).
    pub fn can_run_llm(&self, model: &str, role: Role) -> bool {
        let cfg = self.cfg();
        catalog::resolve_llm(model, &cfg.runtime).is_some() && Self::llama_build_for(&self.gpu_for(role)).is_some()
    }

    /// Make sure `model` is running (on its role's GPU) and hold it there; returns its port.
    pub async fn ensure_llm(self: &Arc<Self>, model: &str, role: Role) -> Result<(u16, Lease), RtError> {
        let cfg = self.cfg();
        let files = catalog::resolve_llm(model, &cfg.runtime)
            .ok_or_else(|| RtError::NotHere(format!("{model} isn't downloaded on this computer")))?;
        let gpu = self.gpu_for(role);
        let (backend, dir) = Self::llama_build_for(&gpu)
            .ok_or_else(|| RtError::NotHere(format!("llama.cpp isn't installed for {}", gpu.label())))?;
        let key = format!("llm:{model}");
        let need = files.vram_mb(cfg.runtime.context);
        let lease = self.arbiter.acquire(&gpu.id, self.budget_mb(&gpu), &key, need, None, |k| self.stop(k)).await;
        let _start = self.starting.lock().await;
        if let Some(port) = self.alive(&key) {
            return Ok((port, lease));
        }
        let mut args: Vec<String> = vec!["-m".into(), files.model.display().to_string()];
        if let Some(p) = &files.mmproj {
            args.extend(["--mmproj".into(), p.display().to_string()]);
        }
        if let Some(d) = &files.draft {
            args.extend(["-md".into(), d.display().to_string()]);
            if files.source != "file" {
                args.extend(["--spec-type".into(), "draft-mtp".into()]);
            }
        }
        if let Some(dev) = gpu.llama_device(backend) {
            args.extend(["--device".into(), dev.clone()]);
            if files.draft.is_some() {
                args.extend(["--device-draft".into(), dev]);
            }
        }
        let ngl = if gpu.id == "cpu" { "0" } else { "999" };
        args.extend(
            ["-ngl", ngl, "-c", &cfg.runtime.context.to_string(), "--jinja", "--host", "127.0.0.1", "--alias", model]
                .map(String::from),
        );
        args.extend(files.args.iter().cloned());
        let port = self
            .spawn(&key, &dir.join(install::exe("llama-server")), args, "/health", Duration::from_secs(900))
            .await
            .map_err(|e| {
                self.arbiter.remove(&key);
                RtError::Failed(e)
            })?;
        tracing::info!("{model} is running on {} ({}) at port {port}", gpu.label(), backend.as_str());
        Ok((port, lease))
    }

    // --- image and video models -------------------------------------------------------------

    fn sd_build_for(gpu: &Gpu) -> Option<(Backend, PathBuf)> {
        let wanted = install::llama_backend_for(gpu.backend());
        if let Some(dir) = install::installed_sd(wanted) {
            return Some((wanted, dir));
        }
        if wanted == Backend::Cuda && gpu.vulkan.is_some() {
            return install::installed_sd(Backend::Vulkan).map(|d| (Backend::Vulkan, d));
        }
        None
    }

    pub fn can_run_sd(&self, role: Role) -> bool {
        let Some(spec) = catalog::sd_spec(&self.model_for(role)) else { return false };
        catalog::sd_installed(spec) && Self::sd_build_for(&self.gpu_for(role)).is_some()
    }

    /// Make sure the image (or video) model is loaded on its GPU; returns the server's port.
    pub async fn ensure_sd(self: &Arc<Self>, role: Role) -> Result<(u16, Lease, &'static SdSpec), RtError> {
        let name = self.model_for(role);
        let spec = catalog::sd_spec(&name).ok_or_else(|| {
            RtError::NotHere(if name.is_empty() {
                format!("{} generation is off on this computer", role.as_str())
            } else {
                format!("unknown {} model {name}", role.as_str())
            })
        })?;
        if !catalog::sd_installed(spec) {
            return Err(RtError::NotHere(format!("{} isn't downloaded on this computer", spec.label)));
        }
        let gpu = self.gpu_for(role);
        let (backend, dir) = Self::sd_build_for(&gpu)
            .ok_or_else(|| RtError::NotHere(format!("stable-diffusion.cpp isn't installed for {}", gpu.label())))?;
        let prefix = format!("sd:{}:", gpu.id);
        let key = format!("{prefix}{}", spec.name);
        let need = (spec.vram_gb * 1024.0) as u64;
        let lease =
            self.arbiter.acquire(&gpu.id, self.budget_mb(&gpu), &key, need, Some(&prefix), |k| self.stop(k)).await;
        let _start = self.starting.lock().await;
        if let Some(port) = self.alive(&key) {
            return Ok((port, lease, spec));
        }
        let mut args: Vec<String> = vec!["--model-dir".into(), catalog::sd_dir().display().to_string()];
        if let Some(dev) = gpu.sd_device(backend) {
            args.extend(["--backend".into(), dev]);
        }
        let started = self
            .spawn(&key, &dir.join(install::exe("sd-server")), args, "/sdcpp/v1/models/status", Duration::from_secs(60))
            .await;
        let port = match started {
            Ok(p) => p,
            Err(e) => {
                self.arbiter.remove(&key);
                return Err(RtError::Failed(e));
            }
        };
        let load = json!({"recipe": spec.recipe, "models": catalog::sd_selection(spec)});
        let url = format!("http://127.0.0.1:{port}/sdcpp/v1/models/load");
        let resp = self.client.post(&url).json(&load).timeout(Duration::from_secs(900)).send().await;
        let ok = match resp {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(format!("loading {} failed: {}", spec.label, r.text().await.unwrap_or_default())),
            Err(e) => Err(format!("loading {} failed: {e}", spec.label)),
        };
        if let Err(e) = ok {
            self.stop(key.clone()).await;
            self.arbiter.remove(&key);
            return Err(RtError::Failed(e));
        }
        tracing::info!("{} is loaded on {} ({}) at port {port}", spec.label, gpu.label(), backend.as_str());
        Ok((port, lease, spec))
    }

    // --- processes ----------------------------------------------------------------------------

    fn alive(&self, key: &str) -> Option<u16> {
        let mut procs = self.procs.lock().unwrap();
        let p = procs.get_mut(key)?;
        match p.child.try_wait() {
            Ok(None) => Some(p.port),
            _ => {
                procs.remove(key);
                None
            }
        }
    }

    fn log_path(key: &str) -> PathBuf {
        let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
        install::home().join("logs").join(format!("{safe}.log"))
    }

    /// Start a server process on a free port and wait until `health` answers 200.
    async fn spawn(
        &self,
        key: &str,
        bin: &std::path::Path,
        mut args: Vec<String>,
        health: &str,
        limit: Duration,
    ) -> Result<u16, String> {
        let port = free_port().ok_or("no free port")?;
        let sd = bin.file_name().is_some_and(|n| n.to_string_lossy().starts_with("sd-server"));
        args.extend(if sd {
            ["--listen-port".to_string(), port.to_string()]
        } else {
            ["--port".to_string(), port.to_string()]
        });
        let log = Self::log_path(key);
        let _ = std::fs::create_dir_all(log.parent().unwrap());
        let out = std::fs::File::create(&log).map_err(|e| format!("can't write {}: {e}", log.display()))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = install::command(bin);
        cmd.args(&args).stdin(std::process::Stdio::null()).stdout(out).stderr(err).kill_on_drop(true);
        tracing::info!("starting {} {}", bin.display(), args.join(" "));
        let child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", bin.display()))?;
        self.procs.lock().unwrap().insert(key.to_string(), Proc { child, port, started: Instant::now() });
        let url = format!("http://127.0.0.1:{port}{health}");
        let began = Instant::now();
        while began.elapsed() < limit {
            if self.alive(key).is_none() {
                return Err(format!("{} stopped while starting: {}", key, log_tail(&log)));
            }
            if let Ok(r) = self.client.get(&url).timeout(Duration::from_secs(5)).send().await
                && r.status().is_success()
            {
                return Ok(port);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        self.stop(key.to_string()).await;
        Err(format!("{key} didn't become ready in {}s: {}", limit.as_secs(), log_tail(&log)))
    }

    /// Stop a model's process.
    async fn stop(&self, key: String) {
        let proc = self.procs.lock().unwrap().remove(&key);
        if let Some(mut p) = proc {
            tracing::info!("stopping {key} (ran {} min)", p.started.elapsed().as_secs() / 60);
            terminate(&mut p.child);
            if tokio::time::timeout(Duration::from_secs(10), p.child.wait()).await.is_err() {
                let _ = p.child.kill().await;
            }
        }
    }

    /// Stop models nobody has used for `idle_minutes`.
    async fn reap(&self) {
        let minutes = self.cfg().runtime.idle_minutes;
        if minutes == 0 {
            return;
        }
        for key in self.arbiter.take_idle(Some(Duration::from_secs(minutes * 60)), None) {
            self.stop(key).await;
        }
    }

    /// Stop a model now if it's idle (all idle models when `model` is empty).
    pub async fn unload(&self, model: &str) {
        let keys = if model.is_empty() {
            self.arbiter.take_idle(None, None)
        } else {
            let mut keys = self.arbiter.take_idle(None, Some(&format!("llm:{model}")));
            for (_, o) in self.arbiter.snapshot() {
                if o.key.starts_with("sd:") && o.key.ends_with(&format!(":{model}")) {
                    keys.extend(self.arbiter.take_idle(None, Some(&o.key)));
                }
            }
            keys
        };
        for key in keys {
            self.stop(key).await;
        }
    }

    /// Stop everything (on quit).
    pub async fn shutdown(&self) {
        let keys: Vec<String> = self.procs.lock().unwrap().keys().cloned().collect();
        for key in keys {
            self.arbiter.remove(&key);
            self.stop(key).await;
        }
    }

    /// What's loaded where: [{name, kind, gpu, vram_mb, active}].
    pub fn loaded(&self) -> Vec<Value> {
        self.arbiter
            .snapshot()
            .into_iter()
            .map(|(gpu, o)| {
                let (kind, name) = match o.key.split_once(':') {
                    Some(("llm", m)) => ("llm", m.to_string()),
                    Some(("sd", rest)) => ("sd", rest.rsplit(':').next().unwrap_or(rest).to_string()),
                    _ => ("", o.key.clone()),
                };
                json!({"name": name, "kind": kind, "gpu": gpu, "vram_mb": o.vram_mb, "active": o.active})
            })
            .collect()
    }

    // --- remotes ------------------------------------------------------------------------------

    /// A remote's kind ("fane", "openai", "sd") and models, asking it at most once a minute.
    pub async fn remote_info(&self, name: &str) -> Result<(String, Vec<String>), String> {
        if let Some((when, kind, models)) = self.remotes.lock().unwrap().get(name)
            && when.elapsed() < Duration::from_secs(60)
        {
            return Ok((kind.clone(), models.clone()));
        }
        let cfg = self.cfg();
        let remote = cfg.remotes.get(name).ok_or_else(|| format!("no remote named {name} in [remotes]"))?;
        let found = crate::discovery::identify(&remote.url, &remote.token, &remote.kind).await?;
        let entry = (found.kind.clone(), found.models.clone());
        self.remotes.lock().unwrap().insert(name.to_string(), (Instant::now(), entry.0.clone(), entry.1.clone()));
        Ok(entry)
    }

    pub fn forget_remote_cache(&self, name: &str) {
        self.remotes.lock().unwrap().remove(name);
    }

    /// Everything about this computer's models, for Settings and other computers.
    pub fn info(&self) -> Value {
        let cfg = self.cfg();
        let gpus = self.gpus();
        let roles: serde_json::Map<String, Value> = Role::ALL
            .into_iter()
            .map(|role| {
                let model = self.model_for(role);
                let gpu = self.gpu_for(role);
                let ready = match role {
                    Role::Chat | Role::Code => self.can_run_llm(&model, role),
                    Role::Image | Role::Video => self.can_run_sd(role),
                };
                (
                    role.as_str().to_string(),
                    json!({"model": model, "gpu": gpu.id, "gpu_label": gpu.label(), "ready": ready}),
                )
            })
            .collect();
        json!({
            "fane": env!("CARGO_PKG_VERSION"),
            "name": hostname(),
            "runtime": format!("llama.cpp {} / stable-diffusion.cpp {}", install::LLAMA_BUILD, install::SD_TAG),
            "gpus": gpus.iter().map(|g| json!({"id": g.id, "name": g.name, "label": g.label(), "vram_mb": g.vram_mb})).collect::<Vec<_>>(),
            "roles": roles,
            "models": catalog::available_llms(&cfg.runtime),
            "image": self.can_run_sd(Role::Image),
            "video": self.can_run_sd(Role::Video),
            "loaded": self.loaded(),
            "share": cfg.runtime.share,
        })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        for (_, mut p) in self.procs.lock().unwrap().drain() {
            terminate(&mut p.child);
        }
    }
}

/// Ask a process to exit (SIGTERM on Unix, so llama.cpp frees the GPU cleanly).
fn terminate(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc_kill(pid as i32, 15);
        }
        return;
    }
    let _ = child.start_kill();
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

fn log_tail(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" / ")
}

pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this computer".into())
}
