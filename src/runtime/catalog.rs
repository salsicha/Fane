//! The models Fane knows how to run, and where their files are: Ollama's downloads if it has
//! them (no second copy), else Fane's own downloads in `~/.fane/models`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::download::{self, Progress};
use super::install::home;
use crate::config::{LocalModelCfg, RuntimeCfg};

/// A built-in language model: its names in Ollama, and its files on Hugging Face.
pub struct LlmSpec {
    pub name: &'static str,
    pub ollama: &'static [&'static str],
    pub file: &'static str,
    pub mmproj: &'static str,
    pub draft: &'static str,
}

pub const LLMS: [LlmSpec; 2] = [
    LlmSpec {
        name: "gemma4:26b",
        ollama: &["gemma4:26b"],
        file: "hf:ggml-org/gemma-4-26B-A4B-it-GGUF/gemma-4-26B-A4B-it-Q4_0.gguf",
        mmproj: "hf:ggml-org/gemma-4-26B-A4B-it-GGUF/mmproj-gemma-4-26B-A4B-it-Q8_0.gguf",
        draft: "hf:ggml-org/gemma-4-26B-A4B-it-GGUF/mtp-gemma-4-26B-A4B-it-Q8_0.gguf",
    },
    LlmSpec {
        name: "qwen3.8:27b",
        ollama: &["qwen3.8:27b", "qwen3.8:latest"],
        file: "hf:unsloth/Qwen3.8-27B-GGUF/Qwen3.8-27B-UD-Q4_K_M.gguf",
        mmproj: "hf:unsloth/Qwen3.8-27B-GGUF/mmproj-F16.gguf",
        draft: "hf:unsloth/Qwen3.8-27B-GGUF/MTP/mtp-Qwen3.8-27B-Q4_0.gguf",
    },
];

/// Image and video models for stable-diffusion.cpp: a server recipe and its files by slot.
pub struct SdSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub video: bool,
    pub recipe: &'static str,
    /// (slot, folder under models/sd, Hugging Face file)
    pub files: &'static [(&'static str, &'static str, &'static str)],
    /// GPU memory while generating, in GB (weights plus working buffers).
    pub vram_gb: f64,
}

pub const SD_MODELS: [SdSpec; 2] = [
    SdSpec {
        name: "z-image-turbo",
        label: "Z-Image Turbo (images)",
        video: false,
        recipe: "z-image-turbo",
        files: &[
            ("diffusion_model", "diffusion_models", "hf:leejet/Z-Image-Turbo-GGUF/z_image_turbo-Q8_0.gguf"),
            ("llm", "text_encoders", "hf:unsloth/Qwen3-4B-Instruct-2507-GGUF/Qwen3-4B-Instruct-2507-Q8_0.gguf"),
            ("vae", "vae", "hf:Comfy-Org/z_image_turbo/split_files/vae/ae.safetensors"),
        ],
        vram_gb: 14.0,
    },
    SdSpec {
        name: "wan2.2-5b",
        label: "Wan 2.2 TI2V 5B (video)",
        video: true,
        recipe: "wan22-ti2v-5b",
        files: &[
            ("diffusion_model", "diffusion_models", "hf:QuantStack/Wan2.2-TI2V-5B-GGUF/Wan2.2-TI2V-5B-Q8_0.gguf"),
            ("t5xxl", "text_encoders", "hf:city96/umt5-xxl-encoder-gguf/umt5-xxl-encoder-Q8_0.gguf"),
            ("vae", "vae", "hf:Comfy-Org/Wan_2.2_ComfyUI_Repackaged/split_files/vae/wan2.2_vae.safetensors"),
        ],
        vram_gb: 20.0,
    },
];

pub fn sd_spec(name: &str) -> Option<&'static SdSpec> {
    SD_MODELS.iter().find(|s| s.name == name)
}

pub fn llm_spec(name: &str) -> Option<&'static LlmSpec> {
    LLMS.iter().find(|s| s.name == name || s.ollama.contains(&name))
}

pub fn llm_dir() -> PathBuf {
    home().join("models/llm")
}

pub fn sd_dir() -> PathBuf {
    home().join("models/sd")
}

/// Where a file reference lives once downloaded (or the path itself).
fn local_path(reference: &str) -> PathBuf {
    match reference.strip_prefix("hf:") {
        Some(rest) => llm_dir().join(rest.rsplit('/').next().unwrap_or(rest)),
        None => crate::config::expand(reference),
    }
}

/// A language model's files on disk.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LlmFiles {
    pub model: PathBuf,
    pub mmproj: Option<PathBuf>,
    pub draft: Option<PathBuf>,
    /// "ollama" or "fane" (or "file" for a configured path).
    pub source: &'static str,
    pub args: Vec<String>,
    pub vram_gb: f64,
}

impl LlmFiles {
    /// GPU memory to plan for, in MiB: the weights, plus the context and working buffers.
    pub fn vram_mb(&self, context: u32) -> u64 {
        if self.vram_gb > 0.0 {
            return (self.vram_gb * 1024.0) as u64;
        }
        let size = |p: &Option<PathBuf>| p.as_ref().and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len());
        let weights = std::fs::metadata(&self.model).map_or(0, |m| m.len()) + size(&self.mmproj) + size(&self.draft);
        let mb = weights / 1024 / 1024;
        mb + mb / 10 + 1024 + (context as u64 / 32768) * 2048
    }
}

/// Where a model's files are, if they're on this computer.
pub fn resolve_llm(name: &str, cfg: &RuntimeCfg) -> Option<LlmFiles> {
    if let Some(custom) = cfg.models.get(name) {
        return resolve_custom(custom);
    }
    let spec = llm_spec(name);
    if cfg.reuse_ollama {
        let wanted: Vec<&str> = match spec {
            Some(s) => s.ollama.to_vec(),
            None => vec![name],
        };
        for w in wanted {
            if let Some(mut files) = ollama_model(w) {
                // Ollama's copy may lack the draft model; use Fane's if it was downloaded.
                if files.draft.is_none()
                    && let Some(s) = spec
                {
                    files.draft = Some(local_path(s.draft)).filter(|p| p.is_file());
                }
                return Some(files);
            }
        }
    }
    let spec = spec?;
    let model = local_path(spec.file);
    model.is_file().then(|| LlmFiles {
        model,
        mmproj: Some(local_path(spec.mmproj)).filter(|p| p.is_file()),
        draft: Some(local_path(spec.draft)).filter(|p| p.is_file()),
        source: "fane",
        args: vec![],
        vram_gb: 0.0,
    })
}

fn resolve_custom(c: &LocalModelCfg) -> Option<LlmFiles> {
    let model = local_path(&c.file);
    let opt = |r: &str| (!r.is_empty()).then(|| local_path(r)).filter(|p| p.is_file());
    model.is_file().then(|| LlmFiles {
        model,
        mmproj: opt(&c.mmproj),
        draft: opt(&c.draft),
        source: "file",
        args: c.args.clone(),
        vram_gb: c.vram_gb,
    })
}

/// Files to download so `name` can run here (empty if it already can, or isn't known).
/// `draft_only`: the model itself is there (e.g. from Ollama) but the draft that speeds it up
/// isn't.
pub fn missing_llm_files(name: &str, cfg: &RuntimeCfg) -> Vec<(String, PathBuf)> {
    let custom = cfg.models.get(name);
    let refs: Vec<String> = match (custom, llm_spec(name)) {
        (Some(c), _) => [&c.file, &c.mmproj, &c.draft].into_iter().filter(|r| r.starts_with("hf:")).cloned().collect(),
        (None, Some(s)) => match resolve_llm(name, cfg) {
            Some(f) if f.draft.is_some() => vec![],
            Some(_) => vec![s.draft.to_string()],
            None => vec![s.file.to_string(), s.mmproj.to_string(), s.draft.to_string()],
        },
        (None, None) => vec![],
    };
    refs.into_iter().map(|r| (r.clone(), local_path(&r))).filter(|(_, p)| !p.is_file()).collect()
}

pub async fn download_llm(name: &str, cfg: &RuntimeCfg, progress: Progress<'_>) -> Result<(), String> {
    for (reference, dest) in missing_llm_files(name, cfg) {
        let url = download::hf_url(&reference).ok_or_else(|| format!("don't know how to download {reference}"))?;
        let file = dest.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        download::fetch(&url, &dest, &file, progress).await?;
    }
    if resolve_llm(name, cfg).is_none() {
        return Err(format!("don't know where to get {name}; add it under [runtime.models] in the config"));
    }
    Ok(())
}

/// Every language model this computer can run: the built-in and configured ones whose files
/// are here, and the rest of Ollama's GGUF models.
pub fn available_llms(cfg: &RuntimeCfg) -> Vec<String> {
    let mut names: Vec<String> = LLMS
        .iter()
        .map(|s| s.name.to_string())
        .chain(cfg.models.keys().cloned())
        .filter(|n| resolve_llm(n, cfg).is_some())
        .collect();
    if cfg.reuse_ollama {
        for (name, _) in ollama_list() {
            let covered = llm_spec(&name).is_some_and(|s| names.iter().any(|n| n == s.name));
            if !covered && !names.contains(&name) && ollama_model(&name).is_some() {
                names.push(name);
            }
        }
    }
    names
}

// --- Ollama's model store ----------------------------------------------------------------

/// Folders where Ollama keeps models on this computer.
pub fn ollama_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Ok(d) = std::env::var("OLLAMA_MODELS") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".ollama/models"));
    }
    if cfg!(target_os = "linux") {
        // The system service (Ollama's install script) runs as the `ollama` user.
        dirs.push(PathBuf::from("/usr/share/ollama/.ollama/models"));
        dirs.push(PathBuf::from("/var/lib/ollama/.ollama/models"));
    }
    dirs.retain(|d| d.join("manifests").is_dir());
    dirs.dedup();
    dirs
}

/// "qwen3.8" → (registry.ollama.ai, library, qwen3.8, latest); "user/model:tag" too.
fn manifest_path(root: &Path, name: &str) -> PathBuf {
    let (repo, tag) = name.split_once(':').unwrap_or((name, "latest"));
    let mut parts: Vec<&str> = repo.split('/').collect();
    if parts.len() == 1 {
        parts.insert(0, "library");
    }
    if parts.len() == 2 {
        parts.insert(0, "registry.ollama.ai");
    }
    let mut p = root.join("manifests");
    for part in parts {
        p = p.join(part);
    }
    p.join(tag)
}

/// A model Ollama has downloaded, if its weights are a GGUF file Fane can read.
pub fn ollama_model(name: &str) -> Option<LlmFiles> {
    for root in ollama_dirs() {
        let Ok(text) = std::fs::read_to_string(manifest_path(&root, name)) else { continue };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let blob = |kind: &str| -> Option<PathBuf> {
            let layer = manifest["layers"].as_array()?.iter().find(|l| l["mediaType"] == kind)?;
            let digest = layer["digest"].as_str()?.replace(':', "-");
            let p = root.join("blobs").join(digest);
            is_gguf(&p).then_some(p)
        };
        let Some(model) = blob("application/vnd.ollama.image.model") else { continue };
        return Some(LlmFiles {
            model,
            mmproj: blob("application/vnd.ollama.image.projector"),
            draft: blob("application/vnd.ollama.image.draft"),
            source: "ollama",
            args: vec![],
            vram_gb: 0.0,
        });
    }
    None
}

/// The models in Ollama's store: (name, manifest path).
pub fn ollama_list() -> Vec<(String, PathBuf)> {
    let mut out = vec![];
    for root in ollama_dirs() {
        let base = root.join("manifests");
        for entry in walkdir::WalkDir::new(&base).min_depth(4).max_depth(4).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel: Vec<String> = entry
                .path()
                .strip_prefix(&base)
                .ok()
                .map(|r| r.iter().map(|c| c.to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            if let [registry, ns, model, tag] = rel.as_slice() {
                let name = match (registry.as_str(), ns.as_str()) {
                    ("registry.ollama.ai", "library") => format!("{model}:{tag}"),
                    ("registry.ollama.ai", ns) => format!("{ns}/{model}:{tag}"),
                    (reg, ns) => format!("{reg}/{ns}/{model}:{tag}"),
                };
                out.push((name, entry.path().to_path_buf()));
            }
        }
    }
    out
}

fn is_gguf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"GGUF"
}

// --- image and video models ----------------------------------------------------------------

/// Where an image/video model file goes: models/sd/<folder>/<file>.
fn sd_path(folder: &str, reference: &str) -> PathBuf {
    sd_dir().join(folder).join(reference.rsplit('/').next().unwrap_or(reference))
}

pub fn sd_installed(spec: &SdSpec) -> bool {
    spec.files.iter().all(|(_, folder, r)| sd_path(folder, r).is_file())
}

/// The server's file ids for each slot (paths relative to the model folder).
pub fn sd_selection(spec: &SdSpec) -> serde_json::Map<String, serde_json::Value> {
    spec.files
        .iter()
        .map(|(slot, folder, r)| {
            let file = r.rsplit('/').next().unwrap_or(r);
            (slot.to_string(), serde_json::Value::String(format!("{folder}/{file}")))
        })
        .collect()
}

pub fn sd_download_size(spec: &SdSpec) -> &'static str {
    if spec.video { "about 13 GB" } else { "about 11 GB" }
}

pub async fn download_sd(spec: &SdSpec, progress: Progress<'_>) -> Result<(), String> {
    for (_, folder, reference) in spec.files {
        let dest = sd_path(folder, reference);
        let url = download::hf_url(reference).ok_or_else(|| format!("bad model reference {reference}"))?;
        let file = dest.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        download::fetch(&url, &dest, &file, progress).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_ollama() -> PathBuf {
        let root = std::env::temp_dir().join(format!("fane-ollama-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let m = root.join("manifests/registry.ollama.ai/library/qwen3.8");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::write(root.join("blobs/sha256-aaa"), b"GGUF....weights").unwrap();
        std::fs::write(root.join("blobs/sha256-bbb"), b"GGUF....proj").unwrap();
        std::fs::write(root.join("blobs/sha256-ccc"), b"not gguf").unwrap();
        std::fs::write(
            m.join("latest"),
            r#"{"layers":[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:aaa"},
                {"mediaType":"application/vnd.ollama.image.projector","digest":"sha256:bbb"},
                {"mediaType":"application/vnd.ollama.image.license","digest":"sha256:ccc"}]}"#,
        )
        .unwrap();
        let x = root.join("manifests/registry.ollama.ai/x/z-image-turbo");
        std::fs::create_dir_all(&x).unwrap();
        std::fs::write(
            x.join("latest"),
            r#"{"layers":[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:ccc"}]}"#,
        )
        .unwrap();
        root
    }

    #[test]
    fn reads_ollama_store() {
        let root = fake_ollama();
        unsafe { std::env::set_var("OLLAMA_MODELS", &root) };
        assert_eq!(manifest_path(&root, "qwen3.8").file_name().unwrap(), "latest");
        assert!(manifest_path(&root, "user/m:7b").ends_with("registry.ollama.ai/user/m/7b"));
        let files = ollama_model("qwen3.8:latest").unwrap();
        assert!(files.model.ends_with("sha256-aaa"));
        assert!(files.mmproj.unwrap().ends_with("sha256-bbb"));
        assert_eq!((files.draft, files.source), (None, "ollama"));
        assert!(ollama_model("x/z-image-turbo").is_none(), "not a GGUF model");
        let names: Vec<String> = ollama_list().into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"qwen3.8:latest".to_string()) && names.contains(&"x/z-image-turbo:latest".to_string()));

        // The built-in qwen3.8:27b is found under Ollama's "latest" tag, and its draft is
        // still to download.
        let cfg = RuntimeCfg { home: root.join("fane").display().to_string(), ..Default::default() };
        super::super::install::set_home(cfg.home_dir());
        assert_eq!(resolve_llm("qwen3.8:27b", &cfg).unwrap().source, "ollama");
        let missing = missing_llm_files("qwen3.8:27b", &cfg);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].0.contains("mtp-Qwen3.8"));
        // A model Ollama doesn't have: all three files.
        if ollama_model("gemma4:26b").is_none() {
            assert_eq!(missing_llm_files("gemma4:26b", &cfg).len(), 3);
        }
        assert!(available_llms(&cfg).contains(&"qwen3.8:27b".to_string()));
        let off = RuntimeCfg { reuse_ollama: false, ..cfg.clone() };
        assert!(resolve_llm("qwen3.8:27b", &off).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sd_selection_ids() {
        let z = sd_spec("z-image-turbo").unwrap();
        let sel = sd_selection(z);
        assert_eq!(sel["diffusion_model"], "diffusion_models/z_image_turbo-Q8_0.gguf");
        assert_eq!(sel["vae"], "vae/ae.safetensors");
        assert!(sd_spec("wan2.2-5b").unwrap().video);
    }

    #[test]
    fn vram_estimate() {
        let dir = std::env::temp_dir().join(format!("fane-vram-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("m.gguf");
        std::fs::File::create(&f).unwrap().set_len(10 * 1024 * 1024 * 1024).unwrap(); // sparse
        let files = LlmFiles { model: f, mmproj: None, draft: None, source: "file", args: vec![], vram_gb: 0.0 };
        assert_eq!(files.vram_mb(32768), 10240 + 1024 + 1024 + 2048);
        assert_eq!(LlmFiles { vram_gb: 3.0, ..files }.vram_mb(32768), 3072);
        std::fs::remove_dir_all(&dir).ok();
    }
}
