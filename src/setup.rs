//! Setup: get llama.cpp (and, if wanted, stable-diffusion.cpp) for this computer's GPUs and
//! the models Fane uses. Models Ollama has already downloaded are used where they are; only
//! what's missing is downloaded, into `~/.fane/models`. Nothing that's already set up is
//! replaced.

use crate::config::{Config, Role};
use crate::runtime::catalog;
use crate::runtime::gpu::{self, Backend, Gpu};
use crate::runtime::install;

pub use crate::runtime::download::Progress;

/// What to set up beyond the everyday model.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Want {
    pub code: bool,
    pub images: bool,
    pub video: bool,
}

/// What setup still has to do on this computer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub llama: Vec<Backend>,
    pub models: Vec<String>,
    pub sd: Vec<Backend>,
    pub media: Vec<String>,
    /// Things worth saying that aren't work (e.g. reusing Ollama's download).
    pub notes: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.llama.is_empty() && self.models.is_empty() && self.sd.is_empty() && self.media.is_empty()
    }

    /// The steps as short phrases.
    pub fn steps(&self, cfg: &Config) -> Vec<String> {
        let mut steps = vec![];
        for b in &self.llama {
            steps.push(format!("get llama.cpp ({})", backend_label(*b)));
        }
        for m in &self.models {
            let missing = catalog::missing_llm_files(m, &cfg.runtime);
            let reused = catalog::resolve_llm(m, &cfg.runtime).is_some();
            steps.push(if reused && !missing.is_empty() {
                format!("download a small speed-up file for {m}")
            } else {
                format!("download {m}{}", size_hint(m))
            });
        }
        for b in &self.sd {
            steps.push(format!("get stable-diffusion.cpp ({})", backend_label(*b)));
        }
        for m in &self.media {
            if let Some(s) = catalog::sd_spec(m) {
                steps.push(format!("download {} ({})", s.label, catalog::sd_download_size(s)));
            }
        }
        steps
    }

    /// One sentence for a confirmation prompt or card.
    pub fn describe(&self, cfg: &Config) -> String {
        let steps = self.steps(cfg);
        let mut s = match steps.len() {
            0 => String::new(),
            1 => steps[0].clone(),
            n => format!("{}, and {}", steps[..n - 1].join(", "), steps[n - 1]),
        };
        if let Some(first) = s.get(..1) {
            s = first.to_uppercase() + &s[1..];
        }
        s
    }
}

fn backend_label(b: Backend) -> &'static str {
    match b {
        Backend::Cuda => "NVIDIA CUDA",
        Backend::Vulkan => "Vulkan",
        Backend::Metal => "Apple Metal",
        Backend::Cpu => "CPU",
    }
}

fn size_hint(model: &str) -> &'static str {
    match model {
        "gemma4:26b" => " (about 16 GB)",
        "qwen3.8:27b" => " (about 19 GB)",
        _ => "",
    }
}

/// Is `host` an Ollama server on this computer (the setting from before Fane ran models
/// itself)?
pub fn is_local_ollama(host: &str) -> bool {
    let h = host.trim();
    !h.is_empty() && is_local(h) && url::Url::parse(h).ok().and_then(|u| u.port()) == Some(11434)
}

pub fn is_local(host: &str) -> bool {
    url::Url::parse(host)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "localhost" || h == "[::1]" || h.starts_with("127.")))
        .unwrap_or(false)
}

/// Whether a role's work happens on this computer (its route starts with "local").
fn local_role(cfg: &Config, role: Role) -> bool {
    cfg.routes.for_role(role).first().is_some_and(|t| t == "local")
}

/// What's missing for the configured models (and the extras in `want`).
pub fn plan(cfg: &Config, want: Want) -> Plan {
    install::set_home(cfg.runtime.home_dir());
    let gpus = gpu::detect();
    plan_with(cfg, want, &gpus)
}

pub fn plan_with(cfg: &Config, want: Want, gpus: &[Gpu]) -> Plan {
    let mut plan = Plan::default();
    let mut llm_roles = vec![];
    if cfg.model.host.trim().is_empty() && local_role(cfg, Role::Chat) {
        llm_roles.push((Role::Chat, cfg.model.name.clone()));
    }
    if want.code && !cfg.model.code.is_empty() {
        llm_roles.push((Role::Code, cfg.model.code.clone()));
    }
    for (role, model) in llm_roles {
        let gpu = gpu::pick(gpus, cfg.runtime.gpu_for(role));
        let backend = install::llama_backend_for(gpu.backend());
        if install::installed_llama(backend).is_none() && !plan.llama.contains(&backend) {
            plan.llama.push(backend);
        }
        let reused = catalog::resolve_llm(&model, &cfg.runtime).filter(|f| f.source == "ollama").is_some();
        if reused {
            plan.notes.push(format!("using Ollama's copy of {model}"));
        }
        let have = catalog::resolve_llm(&model, &cfg.runtime).is_some();
        let missing = !catalog::missing_llm_files(&model, &cfg.runtime).is_empty();
        if (!have || missing) && !plan.models.contains(&model) {
            plan.models.push(model);
        }
    }
    for (role, wanted) in [(Role::Image, want.images), (Role::Video, want.video)] {
        let name = match role {
            Role::Image => &cfg.runtime.image_model,
            _ => &cfg.runtime.video_model,
        };
        let Some(spec) = catalog::sd_spec(name).filter(|_| wanted) else { continue };
        let gpu = gpu::pick(gpus, cfg.runtime.gpu_for(role));
        let backend = install::llama_backend_for(gpu.backend());
        let have_build = install::installed_sd(backend).is_some()
            || (backend == Backend::Cuda && install::installed_sd(Backend::Vulkan).is_some());
        if !have_build && !plan.sd.contains(&backend) {
            plan.sd.push(backend);
        }
        if !catalog::sd_installed(spec) {
            plan.media.push(spec.name.to_string());
        }
    }
    plan
}

/// Carry out `plan`. A CUDA build that can't be had falls back to Vulkan on the same card.
pub async fn run(plan: &Plan, cfg: &Config, progress: Progress<'_>) -> Result<(), String> {
    install::set_home(cfg.runtime.home_dir());
    let build = |s: &str| s.to_string();
    for &backend in &plan.llama {
        match install::install_llama(backend, &build(&cfg.runtime.llama_build), progress).await {
            Ok(_) => {}
            Err(e) if backend == Backend::Cuda => {
                tracing::warn!("{e}; using Vulkan instead");
                progress(&format!("{e}; using Vulkan instead"));
                install::install_llama(Backend::Vulkan, &build(&cfg.runtime.llama_build), progress).await?;
            }
            Err(e) => return Err(e),
        }
    }
    for model in &plan.models {
        catalog::download_llm(model, &cfg.runtime, progress).await?;
    }
    for &backend in &plan.sd {
        match install::install_sd(backend, &build(&cfg.runtime.sd_build), progress).await {
            Ok(_) => {}
            Err(e) if backend == Backend::Cuda => {
                tracing::warn!("{e}; trying Vulkan");
                progress("No CUDA build of stable-diffusion.cpp for this system; using Vulkan…");
                install::install_sd(Backend::Vulkan, &build(&cfg.runtime.sd_build), progress).await?;
            }
            Err(e) => return Err(e),
        }
    }
    for name in &plan.media {
        if let Some(spec) = catalog::sd_spec(name) {
            catalog::download_sd(spec, progress).await?;
        }
    }
    progress("Done.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpus() -> Vec<Gpu> {
        vec![
            Gpu {
                id: "cuda:0".into(),
                name: "V100".into(),
                vram_mb: 32768,
                integrated: false,
                cuda: Some(0),
                vulkan: Some(1),
            },
            Gpu {
                id: "vulkan:0".into(),
                name: "Iris Xe".into(),
                vram_mb: 40000,
                integrated: true,
                cuda: None,
                vulkan: Some(0),
            },
            Gpu::cpu(),
        ]
    }

    fn empty_home() -> Config {
        let mut cfg = Config::default();
        let home = std::env::temp_dir().join(format!("fane-setup-{}", std::process::id()));
        cfg.runtime.home = home.display().to_string();
        cfg.runtime.reuse_ollama = false;
        install::set_home(home);
        cfg
    }

    #[test]
    fn plans_what_is_missing() {
        let mut cfg = empty_home();
        cfg.runtime.chat_gpu = "vulkan:0".into();
        let p = plan_with(&cfg, Want { code: true, images: true, video: false }, &gpus());
        assert_eq!(p.llama, vec![Backend::Vulkan, Backend::Cuda], "chat on the Iris Xe, code on the V100");
        assert_eq!(p.models, vec!["gemma4:26b", "qwen3.8:27b"]);
        assert_eq!(p.sd, vec![Backend::Cuda]);
        assert_eq!(p.media, vec!["z-image-turbo"]);
        let text = p.describe(&cfg);
        assert!(text.starts_with("Get llama.cpp (Vulkan), get llama.cpp (NVIDIA CUDA), download gemma4:26b"), "{text}");
        assert!(text.ends_with("and download Z-Image Turbo (images) (about 11 GB)"), "{text}");
    }

    #[test]
    fn nothing_for_a_server_elsewhere() {
        let mut cfg = empty_home();
        cfg.model.host = "http://192.168.1.20:11434".into();
        assert!(plan_with(&cfg, Want::default(), &gpus()).is_empty());
        cfg.model.host = String::new();
        cfg.routes.chat = vec!["nuc".into()];
        assert!(plan_with(&cfg, Want::default(), &gpus()).is_empty(), "chat runs on another computer");
    }

    #[test]
    fn local_hosts() {
        assert!(is_local("http://127.0.0.1:11434") && is_local("http://localhost:11434"));
        assert!(!is_local("http://192.168.1.20:11434"));
        assert!(is_local_ollama("http://127.0.0.1:11434") && !is_local_ollama("http://127.0.0.1:11500"));
        assert!(!is_local_ollama(""));
    }
}
