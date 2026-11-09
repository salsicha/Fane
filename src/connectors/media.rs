//! Making images and videos: through Fane's model server (this computer's GPU or another
//! computer's, by `[routes]`), or by asking Codex when it's in the image route and no model
//! server can.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use crate::config::{AgentCfg, Config, MediaCfg, Role, expand};
use crate::connectors::delegate::{Vars, exec_status};
use crate::tools::{Args, Tool, handler, params, prop};

pub struct Media {
    pub server: String,
    pub cfg: MediaCfg,
    /// The image route's frontier fallbacks, in order (e.g. ["codex"]).
    pub image_fallbacks: Vec<String>,
    client: reqwest::Client,
}

impl Media {
    pub fn new(cfg: &Config) -> Self {
        let fallbacks = cfg.routes.image.iter().filter(|t| *t == "codex").cloned().collect();
        Self {
            server: cfg.runtime.url(),
            cfg: cfg.media.clone(),
            image_fallbacks: fallbacks,
            client: crate::llm::http_client(Duration::from_secs(3600)),
        }
    }

    fn out_path(&self, prompt: &str, ext: &str) -> PathBuf {
        let dir = expand(&self.cfg.dir);
        let slug: String = prompt
            .chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|w| !w.is_empty())
            .take(6)
            .collect::<Vec<_>>()
            .join("-");
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
        dir.join(format!("{stamp}-{slug}.{ext}"))
    }

    fn save(&self, bytes: &[u8], prompt: &str, ext: &str) -> Result<PathBuf, String> {
        let path = self.out_path(prompt, ext);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| format!("can't save {}: {e}", path.display()))?;
        if self.cfg.open {
            let _ = open::that_detached(&path);
        }
        Ok(path)
    }

    pub async fn image(&self, prompt: &str, size: &str, count: u32) -> Result<Vec<PathBuf>, String> {
        let body = json!({"prompt": prompt, "n": count.clamp(1, 4), "size": size, "output_format": "png"});
        let resp = self
            .client
            .post(format!("{}/v1/images/generations", self.server))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Fane's model server isn't answering: {e}"))?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            let why = v["error"]["message"].as_str().unwrap_or("unknown error").to_string();
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && !self.image_fallbacks.is_empty() {
                return self.image_by_codex(prompt).await.map(|p| vec![p]).map_err(|e| format!("{why}; Codex: {e}"));
            }
            return Err(why);
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut paths = vec![];
        for item in v["data"].as_array().cloned().unwrap_or_default() {
            let bytes = b64.decode(item["b64_json"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
            paths.push(self.save(&bytes, prompt, "png")?);
        }
        if paths.is_empty() { Err("no image came back".into()) } else { Ok(paths) }
    }

    /// Codex makes the image with its own image tool and saves it where we say.
    async fn image_by_codex(&self, prompt: &str) -> Result<PathBuf, String> {
        if which::which("codex").is_err() {
            return Err("not installed".into());
        }
        let path = self.out_path(prompt, "png");
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        let task = format!(
            "Generate an image with your image generation tool: {prompt}\nSave it as a PNG file at exactly this path: {}\nDon't do anything else.",
            path.display()
        );
        let spec = AgentCfg {
            cmd: ["codex", "exec", "--skip-git-repo-check", "--approve-for-me", "{task}"].map(String::from).to_vec(),
            ..Default::default()
        };
        let dir = path.parent().unwrap().to_path_buf();
        let (out, ok) = exec_status("codex", &spec, &task, &dir, &Vars::default(), Duration::from_secs(600)).await;
        if ok && path.is_file() {
            if self.cfg.open {
                let _ = open::that_detached(&path);
            }
            Ok(path)
        } else {
            Err(crate::llm::truncate(&out, 300).to_string())
        }
    }

    pub async fn video(&self, prompt: &str, seconds: f64, size: &str, image: Option<&Path>) -> Result<PathBuf, String> {
        let mut body = json!({"prompt": prompt, "seconds": seconds, "size": size});
        if let Some(p) = image {
            let bytes = std::fs::read(p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
            body["image"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
        let resp = self
            .client
            .post(format!("{}/fane/v1/videos", self.server))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Fane's model server isn't answering: {e}"))?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            return Err(v["error"]["message"].as_str().unwrap_or("unknown error").to_string());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(v["b64_json"].as_str().unwrap_or(""))
            .map_err(|e| e.to_string())?;
        let ext = v["output_format"].as_str().unwrap_or("webm");
        self.save(&bytes, prompt, ext)
    }

    pub fn tools(self: &Arc<Self>) -> Vec<Tool> {
        let me = self.clone();
        let image = Tool::new(
            "generate_image",
            "Make an image from a description and save it (returns the file path; insert_image can paste it).",
            params(
                &["prompt"],
                &[
                    ("prompt", prop("string", "A detailed description of the image")),
                    ("size", prop("string", "WIDTHxHEIGHT, default 1024x1024")),
                    ("count", prop("integer", "How many (1-4)")),
                ],
            ),
            handler(move |a: Args| {
                let me = me.clone();
                async move {
                    let size = if a.str("size").is_empty() { "1024x1024".to_string() } else { a.str("size") };
                    match me.image(&a.require("prompt")?, &size, a.int("count", 1) as u32).await {
                        Ok(paths) => Ok(json!({"saved": paths})),
                        Err(e) => Ok(json!(format!("error: {e}"))),
                    }
                }
            }),
            "Making the image",
        );
        let me = self.clone();
        let video = Tool::new(
            "generate_video",
            "Make a short video clip from a description (optionally starting from an image file) and save it.",
            params(
                &["prompt"],
                &[
                    ("prompt", prop("string", "What happens in the clip")),
                    ("seconds", prop("number", "Length, default 2 (longer takes much longer)")),
                    ("size", prop("string", "WIDTHxHEIGHT, default 832x480")),
                    ("image", prop("string", "Path of an image to start from (optional)")),
                ],
            ),
            handler(move |a: Args| {
                let me = me.clone();
                async move {
                    let size = if a.str("size").is_empty() { "832x480".to_string() } else { a.str("size") };
                    let seconds = a.0.get("seconds").and_then(Value::as_f64).unwrap_or(2.0);
                    let image = a.str("image");
                    let image = (!image.is_empty()).then(|| expand(&image));
                    match me.video(&a.require("prompt")?, seconds, &size, image.as_deref()).await {
                        Ok(path) => Ok(json!({"saved": path})),
                        Err(e) => Ok(json!(format!("error: {e}"))),
                    }
                }
            }),
            "Making the video",
        );
        vec![image, video]
    }
}

/// Whether images (or videos) can be made at all with this config: a route that isn't just
/// unavailable frontier tools.
pub fn route_exists(cfg: &Config, role: Role) -> bool {
    cfg.routes
        .for_role(role)
        .iter()
        .any(|t| t == "local" || cfg.remotes.contains_key(t) || (t == "codex" && which::which("codex").is_ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        let mut cfg = Config::default();
        cfg.media.dir = "/tmp/fane-media".into();
        let m = Media::new(&cfg);
        let p = m.out_path("A cozy reading nook, with a cat!", "png");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(p.starts_with("/tmp/fane-media"));
        assert!(name.ends_with("-a-cozy-reading-nook-with-a.png"), "{name}");
        assert!(route_exists(&cfg, Role::Image));
        cfg.routes.video = vec!["claude".into()];
        assert!(!route_exists(&cfg, Role::Video));
    }
}
