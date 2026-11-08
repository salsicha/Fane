//! Connect to configured stdio MCP servers and expose their tools to the model.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

use crate::config::{McpServerCfg, expand};
use crate::tools::{Args, Tool, ToolRegistry, handler};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const RESULT_MAX: usize = 16_000;

type Client = RunningService<RoleClient, ()>;

/// One configured server and, once started, its live client session.
struct McpServer {
    name: String,
    cfg: McpServerCfg,
    client: Mutex<Option<Arc<Client>>>,
    error: Mutex<Option<String>>,
}

impl McpServer {
    /// The running client, if the session is still open.
    fn client(&self) -> Option<Arc<Client>> {
        self.client.lock().unwrap().clone().filter(|c| !c.is_closed())
    }

    fn set_error(&self, e: impl Into<String>) {
        let e = e.into();
        tracing::warn!("MCP server {} failed: {e}", self.name);
        *self.error.lock().unwrap() = Some(e);
    }

    async fn connect(&self) -> Result<Client, String> {
        let mut cmd = tokio::process::Command::new(resolve_program(&self.cfg.command));
        cmd.args(&self.cfg.args).envs(&self.cfg.env);
        if let Some(cwd) = self.cfg.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
            cmd.current_dir(expand(cwd));
        }
        let (transport, _) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start {:?}: {e}", self.cfg.command))?;
        ().serve(transport).await.map_err(|e| e.to_string())
    }

    async fn start(&self) {
        match tokio::time::timeout(START_TIMEOUT, self.connect()).await {
            Ok(Ok(client)) => *self.client.lock().unwrap() = Some(Arc::new(client)),
            Ok(Err(e)) => self.set_error(e),
            Err(_) => self.set_error("timed out starting"),
        }
    }

    async fn stop(&self) {
        let Some(client) = self.client.lock().unwrap().take() else { return };
        match Arc::try_unwrap(client) {
            Ok(client) => {
                let _ = tokio::time::timeout(STOP_TIMEOUT, client.cancel()).await;
            }
            // A tool call is still in flight; cancelling the token ends the session.
            Err(shared) => shared.cancellation_token().cancel(),
        }
    }
}

/// Bare command names are looked up on PATH (finds `npx.cmd`-style shims on Windows).
fn resolve_program(command: &str) -> PathBuf {
    let bare = !command.contains('/') && !command.contains('\\');
    if bare && let Ok(p) = which::which(command) {
        return p;
    }
    PathBuf::from(command)
}

pub struct McpManager {
    servers: Vec<Arc<McpServer>>,
}

impl McpManager {
    pub fn new(servers: &BTreeMap<String, McpServerCfg>) -> Self {
        let servers = servers
            .iter()
            .filter(|(_, cfg)| !cfg.command.trim().is_empty())
            .map(|(name, cfg)| {
                Arc::new(McpServer {
                    name: name.clone(),
                    cfg: cfg.clone(),
                    client: Mutex::new(None),
                    error: Mutex::new(None),
                })
            })
            .collect();
        Self { servers }
    }

    /// Names of the configured servers (those with a command).
    #[allow(dead_code)]
    pub fn names(&self) -> Vec<String> {
        self.servers.iter().map(|s| s.name.clone()).collect()
    }

    /// Start every server (30 s timeout each, in parallel) and add their tools to `registry`
    /// as "<server>__<tool>" (non-alphanumerics -> '_'), status "Asking <server>".
    pub async fn start(&self, registry: &mut ToolRegistry) {
        futures::future::join_all(self.servers.iter().map(|s| s.start())).await;
        for server in &self.servers {
            let Some(client) = server.client() else { continue };
            let tools = match client.list_all_tools().await {
                Ok(t) => t,
                Err(e) => {
                    server.set_error(format!("listing tools failed: {e}"));
                    continue;
                }
            };
            for t in &tools {
                registry.add(wrap_tool(server, t));
            }
            tracing::info!("MCP {}: {} tools", server.name, tools.len());
        }
    }

    /// Names of servers that failed, with the error.
    pub fn errors(&self) -> Vec<(String, String)> {
        self.servers.iter().filter_map(|s| s.error.lock().unwrap().clone().map(|e| (s.name.clone(), e))).collect()
    }

    pub async fn stop(&self) {
        futures::future::join_all(self.servers.iter().map(|s| s.stop())).await;
    }
}

fn wrap_tool(server: &Arc<McpServer>, t: &rmcp::model::Tool) -> Tool {
    let tool_name = t.name.to_string();
    let description = t.description.as_deref().filter(|d| !d.is_empty()).unwrap_or(&tool_name);
    let parameters = if t.input_schema.is_empty() {
        json!({"type": "object", "properties": {}})
    } else {
        Value::Object((*t.input_schema).clone())
    };
    let name = format!("{}__{}", safe_name(&server.name), safe_name(&tool_name));
    let description = format!("[{}] {description}", server.name);
    let status = format!("Asking {}", server.name);
    let server = server.clone();
    let h = handler(move |args: Args| {
        let server = server.clone();
        let tool_name = tool_name.clone();
        async move {
            let Some(client) = server.client() else {
                return Ok(Value::String(format!("error: MCP server {} is not running", server.name)));
            };
            let params = CallToolRequestParams::new(tool_name).with_arguments(args.0);
            let result = client.call_tool(params).await?;
            Ok(Value::String(render_result(&result)))
        }
    });
    Tool::new(&name, &description, parameters, h, &status)
}

/// Tool-name-safe: anything but `[A-Za-z0-9_]` becomes '_'.
fn safe_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

/// Flatten a tool result into text for the model; images are saved to temp files.
fn render_result(result: &CallToolResult) -> String {
    let parts: Vec<String> = result
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(t) => t.text.clone(),
            ContentBlock::Image(img) => match save_image(&img.data, &img.mime_type) {
                Ok(path) => format!("[image saved to {}; use insert_image to paste it]", path.display()),
                Err(e) => format!("[image could not be saved: {e}]"),
            },
            other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
        })
        .collect();
    let mut text = parts.join("\n");
    if text.is_empty() {
        text = "(no content)".into();
    }
    if result.is_error == Some(true) {
        text = format!("error: {text}");
    }
    text.chars().take(RESULT_MAX).collect()
}

/// Write base64 image data to a fresh `fane-mcp-*.<ext>` file in the temp dir.
fn save_image(data: &str, mime_type: &str) -> anyhow::Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim())?;
    let ext: String = mime_type
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        .collect();
    let ext = if ext.is_empty() { "png".to_string() } else { ext };
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fane-mcp-{}-{nanos:x}{n}.{ext}", std::process::id()));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&path) {
            Ok(mut f) => {
                f.write_all(&bytes)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(safe_name("my-server.v2"), "my_server_v2");
        assert_eq!(safe_name("ok_name_9"), "ok_name_9");
        assert_eq!(safe_name("é x"), "__x");
    }

    #[test]
    fn renders_results() {
        let r = CallToolResult::success(vec![ContentBlock::text("one"), ContentBlock::text("two")]);
        assert_eq!(render_result(&r), "one\ntwo");
        assert_eq!(render_result(&CallToolResult::success(vec![])), "(no content)");
        assert_eq!(render_result(&CallToolResult::error(vec![ContentBlock::text("bad")])), "error: bad");
        let long = CallToolResult::success(vec![ContentBlock::text("x".repeat(20_000))]);
        assert_eq!(render_result(&long).len(), 16_000);

        // 1x1 transparent PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let text = render_result(&CallToolResult::success(vec![ContentBlock::image(png, "image/png")]));
        let path = text
            .strip_prefix("[image saved to ")
            .and_then(|s| s.strip_suffix("; use insert_image to paste it]"))
            .expect(&text);
        assert!(path.ends_with(".png") && path.contains("fane-mcp-"));
        assert_eq!(&std::fs::read(path).unwrap()[1..4], b"PNG");
        std::fs::remove_file(path).unwrap();

        let other = render_result(&CallToolResult::success(vec![ContentBlock::embedded_text("file:///a", "hello")]));
        assert!(other.contains("file:///a") && other.contains("hello"), "{other}");
    }

    #[tokio::test]
    async fn failed_servers_are_reported() {
        let mut servers = BTreeMap::new();
        servers.insert("empty".to_string(), McpServerCfg::default());
        servers.insert(
            "broken".to_string(),
            McpServerCfg { command: "fane-no-such-mcp-server".into(), ..Default::default() },
        );
        let mgr = McpManager::new(&servers);
        assert_eq!(mgr.names(), vec!["broken"]);
        let mut reg = ToolRegistry::new();
        mgr.start(&mut reg).await;
        assert!(reg.names().is_empty());
        let errs = mgr.errors();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].0, "broken");
        mgr.stop().await;
    }

    /// A dependency-free stdio MCP server (raw JSON-RPC lines) for the round-trip test.
    #[cfg(unix)]
    const FAKE_SERVER: &str = r#"
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method, rid = msg.get("method"), msg["id"]
    if method == "initialize":
        result = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fake", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo-text", "description": "Echo it back",
                             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
                            {"name": "fail", "inputSchema": {}}]}
    elif method == "tools/call":
        p = msg["params"]
        if p["name"] == "fail":
            result = {"content": [{"type": "text", "text": "nope"}], "isError": True}
        else:
            result = {"content": [{"type": "text", "text": "echo: " + p["arguments"].get("text", "") + " " + __import__("os").environ.get("FANE_FAKE", "")}]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "no"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": rid, "result": result}), flush=True)
"#;

    // Unix-only: on Windows `python3` may resolve to the Microsoft Store stub.
    #[cfg(unix)]
    #[tokio::test]
    async fn round_trip_with_stdio_server() {
        let Ok(python) = which::which("python3").or_else(|_| which::which("python")) else {
            eprintln!("skipping: no python on PATH");
            return;
        };
        let mut servers = BTreeMap::new();
        servers.insert(
            "fake.srv".to_string(),
            McpServerCfg {
                command: python.display().to_string(),
                args: vec!["-c".into(), FAKE_SERVER.into()],
                env: BTreeMap::from([("FANE_FAKE".to_string(), "env-ok".to_string())]),
                cwd: Some(std::env::temp_dir().display().to_string()),
            },
        );
        let mgr = McpManager::new(&servers);
        let mut reg = ToolRegistry::new();
        mgr.start(&mut reg).await;
        assert!(mgr.errors().is_empty(), "{:?}", mgr.errors());
        assert_eq!(reg.names(), vec!["fake_srv__echo_text", "fake_srv__fail"]);
        let echo = reg.get("fake_srv__echo_text").unwrap();
        assert_eq!(echo.description, "[fake.srv] Echo it back");
        assert_eq!(echo.parameters["properties"]["text"]["type"], "string");
        let fail = reg.get("fake_srv__fail").unwrap();
        assert_eq!(fail.description, "[fake.srv] fail");
        assert_eq!(fail.parameters, json!({"type": "object", "properties": {}}));

        let args = Args::from_value(json!({"text": "hi"}));
        assert_eq!(reg.call("fake_srv__echo_text", args).await, "echo: hi env-ok");
        assert_eq!(reg.call("fake_srv__fail", Args::default()).await, "error: nope");

        mgr.stop().await;
        assert_eq!(reg.call("fake_srv__fail", Args::default()).await, "error: MCP server fake.srv is not running");
    }

    #[test]
    fn not_running_message() {
        let server = Arc::new(McpServer {
            name: "fs".into(),
            cfg: McpServerCfg::default(),
            client: Mutex::new(None),
            error: Mutex::new(None),
        });
        let mut schema = serde_json::Map::new();
        schema.insert("type".into(), json!("object"));
        let t = rmcp::model::Tool::new("read-file", "Read a file", Arc::new(schema));
        let tool = wrap_tool(&server, &t);
        assert_eq!(tool.name, "fs__read_file");
        assert_eq!(tool.description, "[fs] Read a file");
        assert_eq!(tool.status, "Asking fs");
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let out = rt.block_on((tool.handler)(Args::default())).unwrap();
        assert_eq!(out, "error: MCP server fs is not running");
    }
}
