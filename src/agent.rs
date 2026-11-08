//! Tool-using agent loop on top of the local model.
//!
//! Two calling conventions:
//! - native: Ollama's `tools` API (Gemma 4, Qwen 3.8, ...)
//! - json:   for models without native tool calling (e.g. MiniCPM-V 4.7), the model answers
//!   with {"action": ..., "arguments": ...} under a JSON schema.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::ModelCfg;
use crate::llm::{LlmError, LlmResult, Ollama, encode_image, parse_json};
use crate::snapshot::Snapshot;
use crate::tools::{Args, ToolRegistry};

const MAX_STEPS: usize = 8;

pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Mac"
    } else if cfg!(target_os = "windows") {
        "Windows PC"
    } else {
        "Linux computer"
    }
}

fn system_prompt() -> String {
    format!(
        "You are Fane, a quiet desktop assistant running locally on the user's {os}.
You can see the user's screen and you have tools to read their notes and email, list
their open windows, read the clipboard, insert text or images at the user's text cursor,
and delegate hard or long tasks (especially coding) to Claude Code or Codex.

Rules:
- When the user wants text written, filled in, replied or rewritten where they are typing,
  compose it and call insert_text exactly once with only the final text (no preamble).
- When the user asks a question, just answer it. Don't insert or send anything they
  didn't ask for.
- If the screen already shows the answer, answer from the screen without tools.
- Use tools to look things up instead of guessing, and only the tools you need.
  Never invent emails or notes.
- Delegate programming, multi-file or long research tasks to the `delegate` tool.
- Keep replies short and plain. Answer directly; no filler.
",
        os = os_name()
    )
}

fn json_protocol(tools: &str) -> String {
    format!(
        r#"
You call tools by replying with JSON only, one action per reply:
  {{"action": "<tool name>", "arguments": {{...}}}}
When you are finished, reply:
  {{"action": "final", "answer": "<your reply to the user>"}}
Tool results come back in the next message. Most requests need zero or one tool: if the
screen or a previous result already answers the request, reply with "final" right away.

Tools:
{tools}
"#
    )
}

const DONE_NOTE: &str = "The text is now at the user's cursor (nothing was sent). Reply with one short line, no tools.";

#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
    pub output: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentResult {
    pub text: String,
    pub inserted: bool,
    pub calls: Vec<ToolCall>,
    pub mode: String,
}

impl AgentResult {
    pub fn steps(&self) -> Vec<&str> {
        self.calls.iter().map(|c| c.name.as_str()).collect()
    }
}

/// Model name -> "native" | "json"; in "auto" mode this is learned on first use.
pub type ToolModes = Arc<Mutex<HashMap<String, String>>>;

pub struct Agent {
    pub llm: Arc<Ollama>,
    pub tools: ToolRegistry,
    pub cfg: ModelCfg,
    pub modes: ToolModes,
}

impl Agent {
    pub fn new(llm: Arc<Ollama>, tools: ToolRegistry, cfg: ModelCfg, modes: ToolModes) -> Self {
        Self { llm, tools, cfg, modes }
    }

    pub fn mode(&self) -> String {
        match self.cfg.tool_mode.as_str() {
            m @ ("native" | "json") => m.to_string(),
            _ => self.modes.lock().unwrap().get(&self.llm.model()).cloned().unwrap_or_else(|| "native".into()),
        }
    }

    fn context_text(snap: &Snapshot, history: &[String]) -> String {
        let mut lines = vec![
            format!("Time: {}", chrono::Local::now().format("%A %Y-%m-%d %H:%M")),
            format!("Focused window: {}", snap.window.label()),
        ];
        if !history.is_empty() {
            let start = history.len().saturating_sub(5);
            lines.push(format!("Recent activity: {}", history[start..].join(" | ")));
        }
        if snap.image.is_none() {
            lines.push("(No screenshot available.)".into());
        }
        lines.join("\n")
    }

    fn first_messages(&self, prompt: &str, snap: &Snapshot, history: &[String], mode: &str) -> Vec<Value> {
        let mut system = system_prompt();
        if mode == "json" {
            let listing: Vec<String> = self
                .tools
                .iter()
                .map(|t| format!("- {}: {} Arguments: {}", t.name, t.description, t.parameters["properties"]))
                .collect();
            system += &json_protocol(&listing.join("\n"));
        }
        let mut user = json!({"role": "user",
                              "content": format!("{}\n\nRequest: {prompt}", Self::context_text(snap, history))});
        if let Some(img) = &snap.image {
            user["images"] = json!([encode_image(img, self.cfg.max_image_side)]);
        }
        vec![json!({"role": "system", "content": system}), user]
    }

    pub async fn run(
        &self,
        prompt: &str,
        snap: &Snapshot,
        history: &[String],
        on_status: &(dyn Fn(&str) + Send + Sync),
    ) -> LlmResult<AgentResult> {
        let mode = self.mode();
        match self.run_mode(prompt, snap, history, on_status, &mode).await {
            Err(LlmError::ToolsUnsupported(model)) => {
                tracing::info!("{model} has no native tools; switching to JSON tool calls");
                self.modes.lock().unwrap().insert(model, "json".into());
                self.run_mode(prompt, snap, history, on_status, "json").await
            }
            other => other,
        }
    }

    async fn run_mode(
        &self,
        prompt: &str,
        snap: &Snapshot,
        history: &[String],
        on_status: &(dyn Fn(&str) + Send + Sync),
        mode: &str,
    ) -> LlmResult<AgentResult> {
        let mut messages = self.first_messages(prompt, snap, history, mode);
        let mut result = AgentResult { mode: mode.into(), ..Default::default() };
        // "final" first: some models treat the enum as a checklist and walk it in order.
        let mut actions = vec!["final".to_string()];
        actions.extend(self.tools.names());
        let schema = json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": actions},
            "arguments": {"type": "object"}, "answer": {"type": "string"}},
            "required": ["action"]});
        let specs = self.tools.specs();

        for _ in 0..MAX_STEPS {
            on_status("Thinking…");
            let (calls, final_text): (Vec<(String, Value)>, String) = if mode == "native" {
                let msg = self.llm.chat(&messages, Some(&specs), None, None).await?;
                let mut kept = json!({"role": msg["role"].as_str().unwrap_or("assistant"),
                                      "content": msg["content"].as_str().unwrap_or("")});
                if msg.get("tool_calls").is_some_and(|c| !c.is_null()) {
                    kept["tool_calls"] = msg["tool_calls"].clone();
                }
                messages.push(kept);
                let calls = msg["tool_calls"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|c| {
                                (
                                    c["function"]["name"].as_str().unwrap_or("").to_string(),
                                    c["function"]["arguments"].clone(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (calls, msg["content"].as_str().unwrap_or("").trim().to_string())
            } else {
                let msg = self.llm.chat(&messages, None, Some(&schema), None).await?;
                let content = msg["content"].as_str().unwrap_or("").to_string();
                messages.push(json!({"role": "assistant", "content": content}));
                let data = parse_json(&content).unwrap_or_else(|_| json!({"action": "final", "answer": content}));
                let action = match &data["action"] {
                    Value::String(s) => s.clone(),
                    Value::Null => "final".into(),
                    v => v.to_string(),
                };
                let answer = match &data["answer"] {
                    Value::String(s) => s.trim().to_string(),
                    Value::Null => String::new(),
                    v => v.to_string(),
                };
                let calls = if action == "final" { vec![] } else { vec![(action, data["arguments"].clone())] };
                (calls, answer)
            };

            if calls.is_empty() {
                result.text = final_text;
                return Ok(result);
            }

            for (name, args) in calls {
                let args = Args::from_value(args);
                let status = self.tools.get(&name).map(|t| t.status.clone()).filter(|s| !s.is_empty());
                on_status(&format!("{}…", status.unwrap_or_else(|| format!("Using {name}"))));
                let args_json = Value::Object(args.0.clone());
                tracing::info!("tool {name}({})", crate::llm::truncate(&args_json.to_string(), 200));
                let output = self.tools.call(&name, args).await;
                if (name == "insert_text" || name == "insert_image") && output.starts_with("inserted") {
                    result.inserted = true;
                }
                if mode == "native" {
                    messages.push(json!({"role": "tool", "tool_name": name, "content": output}));
                } else {
                    messages.push(json!({"role": "user", "content": format!("Result of {name}:\n{output}")}));
                }
                result.calls.push(ToolCall { name, args: args_json, output });
            }

            if result.inserted {
                // The deliverable is already in the user's document; one more turn for a short note.
                let mut note = DONE_NOTE.to_string();
                if mode == "json" {
                    note += r#" Answer with {"action": "final", "answer": ...}."#;
                }
                messages.push(json!({"role": "user", "content": note}));
                let msg = self.llm.chat(&messages, None, (mode == "json").then_some(&schema), None).await?;
                let mut text = msg["content"].as_str().unwrap_or("").to_string();
                if mode == "json"
                    && let Ok(v) = parse_json(&text)
                {
                    text = v["answer"].as_str().unwrap_or("").to_string();
                }
                let text = text.trim();
                result.text = if text.is_empty() { "Inserted.".into() } else { text.into() };
                return Ok(result);
            }
        }
        if result.text.is_empty() {
            result.text = "Stopped after too many steps.".into();
        }
        Ok(result)
    }
}
