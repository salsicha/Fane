//! Minimal async client for an Ollama server.

use std::collections::HashSet;
use std::io::Cursor;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use base64::Engine;
use image::{DynamicImage, imageops::FilterType};
use serde_json::{Value, json};

use crate::config::ModelCfg;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The model has no native tool calling (e.g. MiniCPM-V); use the JSON protocol instead.
    #[error("{0} does not support native tool calling")]
    ToolsUnsupported(String),
    #[error("{0}")]
    Other(String),
}

pub type LlmResult<T> = Result<T, LlmError>;

fn err(msg: impl Into<String>) -> LlmError {
    LlmError::Other(msg.into())
}

/// Downscale and JPEG-encode an image for the model; returns base64.
pub fn encode_image(img: &DynamicImage, max_side: u32) -> String {
    let longest = img.width().max(img.height()).max(1);
    let rgb = if longest > max_side {
        let scale = max_side as f64 / longest as f64;
        let (w, h) = ((img.width() as f64 * scale).round() as u32, (img.height() as f64 * scale).round() as u32);
        img.resize_exact(w.max(1), h.max(1), FilterType::Lanczos3).to_rgb8()
    } else {
        img.to_rgb8()
    };
    let mut buf = Cursor::new(Vec::new());
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 82);
    rgb.write_with_encoder(enc).expect("JPEG encoding into memory can't fail");
    base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
}

/// 'openbmb/minicpm-v4.7:35b-a3b' -> ('minicpm-v4.7', '35b-a3b'); tag '' if absent.
fn parts(name: &str) -> (&str, &str) {
    let (repo, tag) = name.split_once(':').unwrap_or((name, ""));
    (repo.rsplit('/').next().unwrap_or(repo), tag)
}

/// Match a wanted model against `ollama list`, ignoring namespaces and ':latest'.
pub fn find_installed(name: &str, installed: &[String]) -> Option<String> {
    let (repo, tag) = parts(name);
    let tag = if tag.is_empty() { "latest" } else { tag };
    if let Some(m) = installed.iter().find(|m| parts(m) == (repo, tag) || m.as_str() == name) {
        return Some(m.clone());
    }
    if parts(name).1.is_empty() {
        return installed.iter().find(|m| parts(m).0 == repo).cloned();
    }
    None
}

/// The configured model if installed, else the first installed fallback.
pub fn choose_model(cfg: &ModelCfg, installed: &[String]) -> Option<String> {
    std::iter::once(&cfg.name).chain(&cfg.fallbacks).find_map(|c| find_installed(c, installed))
}

/// Parse a JSON object out of a model reply, tolerating chatter around it.
pub fn parse_json(text: &str) -> LlmResult<Value> {
    let text = text.trim();
    if let Ok(v) = serde_json::from_str(text) {
        return Ok(v);
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}'))
        && end > start
        && let Ok(v) = serde_json::from_str(&text[start..=end])
    {
        return Ok(v);
    }
    Err(err(format!("model did not return JSON: {}", truncate(text, 200))))
}

pub fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Stats {
    pub total_duration: u64,
    pub load_duration: u64,
    pub prompt_eval_count: u64,
    pub prompt_eval_duration: u64,
    pub eval_count: u64,
    pub eval_duration: u64,
}

impl Stats {
    pub fn tok_s(&self) -> Option<f64> {
        (self.eval_count > 0 && self.eval_duration > 0)
            .then(|| self.eval_count as f64 / (self.eval_duration as f64 / 1e9))
    }
}

/// Running totals across calls (used by `fane eval`).
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Totals {
    pub calls: u64,
    pub eval_count: u64,
    pub thinking_chars: u64,
}

pub struct Ollama {
    cfg: RwLock<ModelCfg>,
    model: RwLock<String>,
    client: reqwest::Client,
    no_think: Mutex<HashSet<String>>, // models that reject the `think` switch
    pub last_stats: Mutex<Stats>,
    pub totals: Mutex<Totals>,
}

impl Ollama {
    pub fn new(cfg: &ModelCfg) -> Self {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(cfg.timeout)).build().expect("HTTP client");
        Self {
            model: RwLock::new(cfg.name.clone()),
            cfg: RwLock::new(cfg.clone()),
            client,
            no_think: Mutex::new(HashSet::new()),
            last_stats: Mutex::new(Stats::default()),
            totals: Mutex::new(Totals::default()),
        }
    }

    /// The model actually in use (after `resolve_model`).
    pub fn model(&self) -> String {
        self.model.read().unwrap().clone()
    }

    pub fn set_model(&self, name: &str) {
        *self.model.write().unwrap() = name.to_string();
    }

    pub fn host(&self) -> String {
        self.cfg.read().unwrap().host.trim_end_matches('/').to_string()
    }

    /// Point at a different Ollama server (call `resolve_model` afterwards).
    pub fn set_host(&self, host: &str) {
        self.cfg.write().unwrap().host = host.to_string();
    }

    pub fn totals(&self) -> Totals {
        *self.totals.lock().unwrap()
    }

    pub fn stats(&self) -> Stats {
        self.last_stats.lock().unwrap().clone()
    }

    pub async fn get_json(&self, path: &str) -> LlmResult<Value> {
        let url = format!("{}{path}", self.host());
        let r = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| err(format!("Ollama is not reachable at {}: {e}", self.host())))?;
        let r = r.error_for_status().map_err(|e| err(format!("Ollama error: {e}")))?;
        r.json().await.map_err(|e| err(format!("bad reply from Ollama: {e}")))
    }

    pub async fn installed_models(&self) -> LlmResult<Vec<String>> {
        let v = self.get_json("/api/tags").await?;
        Ok(names(&v["models"]))
    }

    /// Use the configured model if installed, else the first installed fallback.
    pub async fn resolve_model(&self) -> LlmResult<String> {
        let installed = self.installed_models().await?;
        let cfg = self.cfg.read().unwrap().clone();
        if let Some(found) = choose_model(&cfg, &installed) {
            if find_installed(&cfg.name, &installed).as_deref() != Some(found.as_str()) {
                tracing::warn!("model {:?} not installed; using {:?}", cfg.name, found);
            }
            self.set_model(&found);
            return Ok(found);
        }
        Err(err(format!(
            "No vision model installed. Try: ollama pull {} (installed: {})",
            cfg.name,
            if installed.is_empty() { "none".into() } else { installed.join(", ") }
        )))
    }

    /// Single non-streaming chat turn. Returns the assistant message object.
    pub async fn chat(
        &self,
        messages: &[Value],
        tools: Option<&[Value]>,
        schema: Option<&Value>,
        temperature: Option<f64>,
    ) -> LlmResult<Value> {
        let cfg = self.cfg.read().unwrap().clone();
        let model = self.model();
        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "keep_alive": cfg.keep_alive,
            "options": {"temperature": temperature.unwrap_or(cfg.temperature), "num_ctx": cfg.num_ctx},
        });
        let think = !self.no_think.lock().unwrap().contains(&model);
        if think {
            body["think"] = json!(false); // suggestions and tool calls want speed, not long reasoning
        }
        if let Some(tools) = tools.filter(|t| !t.is_empty()) {
            body["tools"] = json!(tools);
        }
        if let Some(schema) = schema {
            body["format"] = schema.clone();
            // Format-constrained decoding occasionally degenerates into endless whitespace.
            body["options"]["num_predict"] = json!(cfg.json_max_tokens);
        }
        let (mut status, mut text) = self.post_chat(&body).await?;
        if status == 400 && text.contains("think") && think {
            // Model doesn't know the `think` switch; remember and retry without it.
            self.no_think.lock().unwrap().insert(model.clone());
            body.as_object_mut().unwrap().remove("think");
            (status, text) = self.post_chat(&body).await?;
        }
        if status >= 400 {
            if body.get("tools").is_some() && text.contains("does not support tools") {
                return Err(LlmError::ToolsUnsupported(model));
            }
            return Err(err(format!("Ollama error {status}: {}", truncate(&text, 300))));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| err(format!("bad reply from Ollama: {e}")))?;
        let n = |k: &str| data[k].as_u64().unwrap_or(0);
        *self.last_stats.lock().unwrap() = Stats {
            total_duration: n("total_duration"),
            load_duration: n("load_duration"),
            prompt_eval_count: n("prompt_eval_count"),
            prompt_eval_duration: n("prompt_eval_duration"),
            eval_count: n("eval_count"),
            eval_duration: n("eval_duration"),
        };
        let msg = data["message"].clone();
        let mut t = self.totals.lock().unwrap();
        t.calls += 1;
        t.eval_count += n("eval_count");
        // Some models reason even when asked not to; that's where latency hides.
        t.thinking_chars += msg["thinking"].as_str().map_or(0, |s| s.chars().count() as u64);
        Ok(msg)
    }

    async fn post_chat(&self, body: &Value) -> LlmResult<(u16, String)> {
        let url = format!("{}/api/chat", self.host());
        let r =
            self.client.post(&url).json(body).send().await.map_err(|e| err(format!("Ollama request failed: {e}")))?;
        let status = r.status().as_u16();
        let text = r.text().await.map_err(|e| err(format!("Ollama request failed: {e}")))?;
        Ok((status, text))
    }

    pub async fn chat_json(&self, messages: &[Value], schema: &Value) -> LlmResult<Value> {
        let msg = self.chat(messages, None, Some(schema), None).await?;
        parse_json(msg["content"].as_str().unwrap_or(""))
    }

    /// Ask Ollama to free a model's memory now.
    pub async fn unload_model(&self, name: &str) {
        let url = format!("{}/api/generate", self.host());
        let _ = self.client.post(&url).json(&json!({"model": name, "keep_alive": 0})).send().await;
    }

    pub async fn unload(&self) {
        self.unload_model(&self.model()).await;
    }

    /// Everything Ollama currently has loaded (/api/ps).
    pub async fn running(&self) -> LlmResult<Vec<Value>> {
        let v = self.get_json("/api/ps").await?;
        Ok(v["models"].as_array().cloned().unwrap_or_default())
    }

    /// This model's entry in /api/ps (size and how much of it is in VRAM).
    pub async fn loaded(&self) -> Option<Value> {
        let model = self.model();
        self.running().await.ok()?.into_iter().find(|m| m["name"] == model.as_str() || m["model"] == model.as_str())
    }

    pub async fn version(&self) -> String {
        self.get_json("/api/version")
            .await
            .ok()
            .and_then(|v| v["version"].as_str().map(String::from))
            .unwrap_or_else(|| "?".into())
    }
}

fn names(models: &Value) -> Vec<String> {
    models
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> Vec<String> {
        ["qwen3-vl:latest", "openbmb/minicpm-v4.7:35b-a3b", "gemma4:31b"].map(String::from).to_vec()
    }

    #[test]
    fn find_installed_cases() {
        let i = installed();
        let cases = [
            ("openbmb/minicpm-v4.7:35b-a3b", Some("openbmb/minicpm-v4.7:35b-a3b")),
            ("minicpm-v4.7:35b-a3b", Some("openbmb/minicpm-v4.7:35b-a3b")),
            ("gemma4", Some("gemma4:31b")),
            ("gemma4:26b", None),
            ("qwen3-vl", Some("qwen3-vl:latest")),
            ("qwen3.8:27b", None),
        ];
        for (wanted, expected) in cases {
            assert_eq!(find_installed(wanted, &i).as_deref(), expected, "{wanted}");
        }
    }

    #[test]
    fn choose_model_falls_back() {
        let cfg = ModelCfg {
            name: "openbmb/minicpm-v4.7:35b-a3b".into(),
            fallbacks: vec!["gemma4:31b".into(), "qwen3-vl".into()],
            ..Default::default()
        };
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            choose_model(&cfg, &s(&["minicpm-v4.7:35b-a3b", "gemma4:31b"])).as_deref(),
            Some("minicpm-v4.7:35b-a3b")
        );
        assert_eq!(choose_model(&cfg, &s(&["llama3:8b", "qwen3-vl:latest"])).as_deref(), Some("qwen3-vl:latest"));
        assert_eq!(choose_model(&cfg, &s(&["llama3:8b"])), None);
    }

    #[test]
    fn parse_json_tolerates_wrapping() {
        assert_eq!(parse_json(r#"Sure! {"a": 1} hope that helps"#).unwrap(), json!({"a": 1}));
        assert!(parse_json("no json here").is_err());
    }

    #[test]
    fn encode_image_downscales() {
        let img = DynamicImage::new_rgb8(2560, 1440);
        let b64 = encode_image(&img, 1280);
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let back = image::load_from_memory(&bytes).unwrap();
        assert_eq!((back.width(), back.height()), (1280, 720));
    }
}
