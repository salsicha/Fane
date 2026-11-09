//! Minimal async client for an OpenAI-compatible chat server: Fane's own model server (which
//! runs llama.cpp and routes requests), or Ollama / another server set in `[model] host`.
//!
//! The rest of Fane builds messages in one internal shape (images as base64 in `images`, tool
//! calls with object arguments, tool results with `tool_name`); this module converts.

use std::io::Cursor;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use base64::Engine;
use image::{DynamicImage, imageops::FilterType};
use serde_json::{Value, json};

use crate::config::ModelCfg;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The model has no native tool calling; use the JSON protocol instead.
    #[error("{0} does not support native tool calling")]
    ToolsUnsupported(String),
    #[error("{0}")]
    Other(String),
}

pub type LlmResult<T> = Result<T, LlmError>;

fn err(msg: impl Into<String>) -> LlmError {
    LlmError::Other(msg.into())
}

/// An HTTP client. If the system's CA certificates can't be loaded (a minimal install without
/// `ca-certificates`), plain-HTTP Ollama still works and HTTPS fails with a clear error.
pub fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder().timeout(timeout).build().unwrap_or_else(|e| {
        tracing::warn!("system CA certificates unavailable ({e}); HTTPS servers won't be trusted");
        reqwest::Client::builder()
            .timeout(timeout)
            .tls_certs_only(std::iter::empty())
            .build()
            .expect("HTTP client without TLS roots")
    })
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

/// 'library/qwen3.8:27b' -> ('qwen3.8', '27b'); tag '' if absent.
fn parts(name: &str) -> (&str, &str) {
    let (repo, tag) = name.split_once(':').unwrap_or((name, ""));
    (repo.rsplit('/').next().unwrap_or(repo), tag)
}

/// Match a wanted model against the available ones, ignoring namespaces and ':latest'.
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

pub struct Llm {
    cfg: RwLock<ModelCfg>,
    host: RwLock<String>,
    model: RwLock<String>,
    client: reqwest::Client,
    pub last_stats: Mutex<Stats>,
    pub totals: Mutex<Totals>,
}

impl Llm {
    /// A client for `host` (Fane's model server, or the server in `[model] host`).
    pub fn new(cfg: &ModelCfg, host: &str) -> Self {
        let client = http_client(Duration::from_secs(cfg.timeout));
        Self {
            model: RwLock::new(cfg.name.clone()),
            host: RwLock::new(host.trim_end_matches('/').to_string()),
            cfg: RwLock::new(cfg.clone()),
            client,
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

    /// The server's address, without `/v1`.
    pub fn host(&self) -> String {
        self.host.read().unwrap().clone()
    }

    /// Point at a different server (call `resolve_model` afterwards).
    pub fn set_host(&self, host: &str) {
        *self.host.write().unwrap() = host.trim_end_matches('/').to_string();
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
            .map_err(|e| err(format!("the model server is not reachable at {}: {e}", self.host())))?;
        let r = r.error_for_status().map_err(|e| err(format!("model server error: {e}")))?;
        r.json().await.map_err(|e| err(format!("bad reply from the model server: {e}")))
    }

    /// The models the server can run.
    pub async fn installed_models(&self) -> LlmResult<Vec<String>> {
        let v = self.get_json("/v1/models").await?;
        Ok(v["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
            .unwrap_or_default())
    }

    /// Use the configured model if available, else the first available fallback.
    pub async fn resolve_model(&self) -> LlmResult<String> {
        let installed = self.installed_models().await?;
        let cfg = self.cfg.read().unwrap().clone();
        if let Some(found) = choose_model(&cfg, &installed) {
            if find_installed(&cfg.name, &installed).as_deref() != Some(found.as_str()) {
                tracing::warn!("model {:?} not available; using {:?}", cfg.name, found);
            }
            self.set_model(&found);
            return Ok(found);
        }
        Err(err(format!(
            "{} isn't available. Run `fane setup` to download it (available: {})",
            cfg.name,
            if installed.is_empty() { "none".into() } else { installed.join(", ") }
        )))
    }

    /// Single non-streaming chat turn. Returns the assistant message in Fane's internal shape.
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
            "messages": to_openai(messages),
            "stream": false,
            "temperature": temperature.unwrap_or(cfg.temperature),
            // Suggestions and tool calls want speed, not long reasoning.
            "chat_template_kwargs": {"enable_thinking": false},
        });
        if let Some(tools) = tools.filter(|t| !t.is_empty()) {
            body["tools"] = json!(tools);
        }
        if let Some(schema) = schema {
            body["response_format"] =
                json!({"type": "json_schema", "json_schema": {"name": "reply", "schema": schema}});
            // Format-constrained decoding occasionally degenerates into endless whitespace.
            body["max_tokens"] = json!(cfg.json_max_tokens);
        }
        let started = std::time::Instant::now();
        let url = format!("{}/v1/chat/completions", self.host());
        let r =
            self.client.post(&url).json(&body).send().await.map_err(|e| err(format!("model request failed: {e}")))?;
        let status = r.status().as_u16();
        let text = r.text().await.map_err(|e| err(format!("model request failed: {e}")))?;
        if status >= 400 {
            if body.get("tools").is_some() && text.to_lowercase().contains("tool") && status < 500 {
                return Err(LlmError::ToolsUnsupported(model));
            }
            return Err(err(format!("model server error {status}: {}", truncate(&text, 300))));
        }
        let data: Value =
            serde_json::from_str(&text).map_err(|e| err(format!("bad reply from the model server: {e}")))?;
        let n = |v: &Value| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)).unwrap_or(0);
        let ms = |v: &Value| (v.as_f64().unwrap_or(0.0) * 1e6) as u64;
        let t = &data["timings"];
        let stats = Stats {
            total_duration: started.elapsed().as_nanos() as u64,
            load_duration: 0,
            prompt_eval_count: n(&data["usage"]["prompt_tokens"]),
            prompt_eval_duration: ms(&t["prompt_ms"]),
            eval_count: n(&data["usage"]["completion_tokens"]),
            eval_duration: ms(&t["predicted_ms"]),
        };
        let msg = from_openai(&data["choices"][0]["message"]);
        let mut totals = self.totals.lock().unwrap();
        totals.calls += 1;
        totals.eval_count += stats.eval_count;
        // Some models reason even when asked not to; that's where latency hides.
        totals.thinking_chars += msg["thinking"].as_str().map_or(0, |s| s.chars().count() as u64);
        *self.last_stats.lock().unwrap() = stats;
        Ok(msg)
    }

    pub async fn chat_json(&self, messages: &[Value], schema: &Value) -> LlmResult<Value> {
        let msg = self.chat(messages, None, Some(schema), None).await?;
        parse_json(msg["content"].as_str().unwrap_or(""))
    }

    /// Ask Fane's model server to stop a model now, freeing its memory.
    pub async fn unload_model(&self, name: &str) {
        let url = format!("{}/fane/v1/unload", self.host());
        let _ = self.client.post(&url).json(&json!({"model": name})).send().await;
    }

    pub async fn unload(&self) {
        self.unload_model(&self.model()).await;
    }

    /// What the server has loaded: [{name, gpu, vram_mb}].
    pub async fn running(&self) -> LlmResult<Vec<Value>> {
        let v = self.get_json("/fane/v1/info").await?;
        Ok(v["loaded"].as_array().cloned().unwrap_or_default())
    }

    /// This model's entry in `running`.
    pub async fn loaded(&self) -> Option<Value> {
        let model = self.model();
        self.running().await.ok()?.into_iter().find(|m| m["name"] == model.as_str())
    }

    pub async fn version(&self) -> String {
        match self.get_json("/fane/v1/info").await {
            Ok(v) => v["runtime"].as_str().unwrap_or("?").to_string(),
            Err(_) => "?".into(),
        }
    }
}

/// Fane's internal messages → OpenAI chat messages. Tool calls get ids so each tool result can
/// point back at its call.
pub fn to_openai(messages: &[Value]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len());
    let mut pending: Vec<(String, String)> = vec![]; // (id, tool name) awaiting a result
    let mut seq = 0;
    for m in messages {
        let role = m["role"].as_str().unwrap_or("user");
        let content = m["content"].as_str().unwrap_or("");
        match role {
            "assistant" => {
                let mut o = json!({"role": "assistant", "content": content});
                if let Some(calls) = m["tool_calls"].as_array().filter(|c| !c.is_empty()) {
                    let calls: Vec<Value> = calls
                        .iter()
                        .map(|c| {
                            seq += 1;
                            let id = c["id"].as_str().map(String::from).unwrap_or_else(|| format!("call_{seq}"));
                            let name = c["function"]["name"].as_str().unwrap_or("").to_string();
                            let args = match &c["function"]["arguments"] {
                                Value::String(s) => s.clone(),
                                Value::Null => "{}".into(),
                                v => v.to_string(),
                            };
                            pending.push((id.clone(), name.clone()));
                            json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}})
                        })
                        .collect();
                    o["tool_calls"] = json!(calls);
                }
                out.push(o);
            }
            "tool" => {
                let name = m["tool_name"].as_str().unwrap_or("");
                let i = pending.iter().position(|(_, n)| n == name).unwrap_or(0);
                let id = if pending.is_empty() { format!("call_{seq}") } else { pending.remove(i).0 };
                out.push(json!({"role": "tool", "tool_call_id": id, "content": content}));
            }
            _ => {
                let images = m["images"].as_array().filter(|i| !i.is_empty());
                let content = match images {
                    Some(images) => {
                        let mut parts = vec![json!({"type": "text", "text": content})];
                        for img in images.iter().filter_map(Value::as_str) {
                            parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{img}")}}));
                        }
                        json!(parts)
                    }
                    None => json!(content),
                };
                out.push(json!({"role": role, "content": content}));
            }
        }
    }
    out
}

/// An OpenAI assistant message → Fane's internal shape (object arguments, `thinking`).
pub fn from_openai(msg: &Value) -> Value {
    let mut o = json!({"role": "assistant", "content": msg["content"].as_str().unwrap_or("")});
    if let Some(r) = msg["reasoning_content"].as_str().filter(|r| !r.is_empty()) {
        o["thinking"] = json!(r);
    }
    if let Some(calls) = msg["tool_calls"].as_array().filter(|c| !c.is_empty()) {
        let calls: Vec<Value> = calls
            .iter()
            .map(|c| {
                let args = match &c["function"]["arguments"] {
                    Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
                    Value::Null => json!({}),
                    v => v.clone(),
                };
                json!({"id": c["id"], "function": {"name": c["function"]["name"], "arguments": args}})
            })
            .collect();
        o["tool_calls"] = json!(calls);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> Vec<String> {
        ["qwen3-vl:latest", "library/qwen3.8:27b", "gemma4:31b"].map(String::from).to_vec()
    }

    #[test]
    fn find_installed_cases() {
        let i = installed();
        let cases = [
            ("library/qwen3.8:27b", Some("library/qwen3.8:27b")),
            ("qwen3.8:27b", Some("library/qwen3.8:27b")),
            ("gemma4", Some("gemma4:31b")),
            ("gemma4:26b", None),
            ("qwen3-vl", Some("qwen3-vl:latest")),
            ("qwen3.8:9b", None),
        ];
        for (wanted, expected) in cases {
            assert_eq!(find_installed(wanted, &i).as_deref(), expected, "{wanted}");
        }
    }

    #[test]
    fn choose_model_falls_back() {
        let cfg = ModelCfg {
            name: "gemma4:26b".into(),
            fallbacks: vec!["gemma4:31b".into(), "qwen3-vl".into()],
            ..Default::default()
        };
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(choose_model(&cfg, &s(&["gemma4:26b", "gemma4:31b"])).as_deref(), Some("gemma4:26b"));
        assert_eq!(choose_model(&cfg, &s(&["llama3:8b", "qwen3-vl:latest"])).as_deref(), Some("qwen3-vl:latest"));
        assert_eq!(choose_model(&cfg, &s(&["llama3:8b"])), None);
    }

    #[test]
    fn parse_json_tolerates_wrapping() {
        assert_eq!(parse_json(r#"Sure! {"a": 1} hope that helps"#).unwrap(), json!({"a": 1}));
        assert!(parse_json("no json here").is_err());
    }

    #[test]
    fn converts_messages_both_ways() {
        let internal = vec![
            json!({"role": "system", "content": "be brief"}),
            json!({"role": "user", "content": "what's this?", "images": ["QUJD"]}),
            json!({"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "read_file", "arguments": {"path": "/a"}}},
                {"function": {"name": "list_files", "arguments": {}}}]}),
            json!({"role": "tool", "tool_name": "list_files", "content": "[]"}),
            json!({"role": "tool", "tool_name": "read_file", "content": "hello"}),
        ];
        let o = to_openai(&internal);
        assert_eq!(o[1]["content"][1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
        assert_eq!(o[2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"/a\"}");
        let id_of = |name: &str| {
            o[2]["tool_calls"].as_array().unwrap().iter().find(|c| c["function"]["name"] == name).unwrap()["id"].clone()
        };
        assert_eq!(o[3]["tool_call_id"], id_of("list_files"));
        assert_eq!(o[4]["tool_call_id"], id_of("read_file"));

        let back = from_openai(&json!({"role": "assistant", "content": null, "reasoning_content": "hmm",
            "tool_calls": [{"id": "x1", "type": "function", "function": {"name": "f", "arguments": "{\"a\":1}"}}]}));
        assert_eq!(back["tool_calls"][0]["function"]["arguments"], json!({"a": 1}));
        assert_eq!((back["content"].as_str(), back["thinking"].as_str()), (Some(""), Some("hmm")));
        // Round trip keeps the server's id.
        assert_eq!(to_openai(&[back])[0]["tool_calls"][0]["id"], "x1");
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
