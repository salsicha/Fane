//! Configuration: the user's `config.toml` merged over built-in defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Recommended local vision models (Ollama tags), in default preference order.
pub const RECOMMENDED_MODELS: [&str; 3] = [
    "openbmb/minicpm-v4.7:35b-a3b", // MiniCPM-V 4.7 35B MoE (3B active), JSON tool calls
    "gemma4:31b",                   // Gemma 4 31B, dense, native tools
    "qwen3.8:27b",                  // Qwen 3.8 27B, dense, native tools
];

pub static CONFIG_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("fane"));
pub static CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| CONFIG_DIR.join("config.toml"));
pub static STATE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    dirs::state_dir().or_else(dirs::data_local_dir).unwrap_or_else(|| home().join(".local/state")).join("fane")
});
/// Private per-session scratch space (screenshots, the IPC socket).
#[cfg_attr(windows, allow(dead_code))]
pub static RUNTIME_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| dirs::runtime_dir().unwrap_or_else(|| std::env::temp_dir().join(format!("fane-{}", whoami()))));

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn whoami() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into())
}

/// Expand `~` and `$VARS` in a configured path.
pub fn expand(path: &str) -> PathBuf {
    let mut s = path.to_string();
    if let Some(rest) = s.strip_prefix("~")
        && (rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
    {
        s = format!("{}{}", home().display(), rest);
    }
    let re = regex::Regex::new(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?").unwrap();
    let s = re.replace_all(&s, |c: &regex::Captures| std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string()));
    PathBuf::from(s.into_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub model: ModelCfg,
    pub suggestions: SuggestionsCfg,
    pub privacy: PrivacyCfg,
    pub hotkey: HotkeyCfg,
    pub insert: InsertCfg,
    pub connectors: ConnectorsCfg,
    pub delegate: DelegateCfg,
    pub agent_mail: AgentMailCfg,
    pub mcp: McpCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCfg {
    /// Ollama server. Pick one on your network in Settings or with `fane ollama use`.
    pub host: String,
    /// Vision-language model served by Ollama. If it isn't installed, the first installed
    /// model from `fallbacks` is used instead.
    pub name: String,
    pub fallbacks: Vec<String>,
    /// Optional different (e.g. faster) model just for proactive suggestions.
    pub suggest_name: String,
    /// "auto" uses Ollama's native tool calling and falls back to a JSON protocol for
    /// models without it (MiniCPM-V 4.7). Force with "native" or "json".
    pub tool_mode: String,
    pub temperature: f64,
    pub keep_alive: String,
    /// A modest context keeps the model fully on the GPU.
    pub num_ctx: u32,
    pub json_max_tokens: u32,
    pub max_image_side: u32,
    pub timeout: u64,
}

impl Default for ModelCfg {
    fn default() -> Self {
        let mut fallbacks: Vec<String> = RECOMMENDED_MODELS.iter().map(|s| s.to_string()).collect();
        fallbacks.extend(
            ["minicpm-v4.7", "gemma4", "qwen3.8", "qwen3-vl", "minicpm-v", "qwen2.5vl", "llava"].map(String::from),
        );
        Self {
            host: "http://127.0.0.1:11434".into(),
            name: RECOMMENDED_MODELS[0].into(),
            fallbacks,
            suggest_name: String::new(),
            tool_mode: "auto".into(),
            temperature: 0.3,
            keep_alive: "15m".into(),
            num_ctx: 8192,
            json_max_tokens: 1536,
            max_image_side: 1280,
            timeout: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SuggestionsCfg {
    pub enabled: bool,
    pub interval_seconds: u64,
    /// Ignore the screen unless it changed at least this much (0..1).
    pub min_change: f64,
    pub min_confidence: f64,
    pub display_seconds: u64,
    /// Base quiet period after a suggestion is dismissed; doubles on repeated dismissals.
    pub cooldown_seconds: u64,
    pub max_cooldown_seconds: u64,
    /// top-right | top-left | bottom-right | bottom-left
    pub corner: String,
}

impl Default for SuggestionsCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_seconds: 25,
            min_change: 0.015,
            min_confidence: 0.65,
            display_seconds: 18,
            cooldown_seconds: 90,
            max_cooldown_seconds: 1800,
            corner: if cfg!(target_os = "windows") { "bottom-right" } else { "top-right" }.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyCfg {
    /// Never capture the screen while one of these apps (app id / process substring) is focused.
    pub blocked_apps: Vec<String>,
    /// ...or while the window title contains one of these.
    pub blocked_titles: Vec<String>,
}

impl Default for PrivacyCfg {
    fn default() -> Self {
        Self {
            blocked_apps: [
                "keepassxc",
                "1password",
                "bitwarden",
                "seahorse",
                "gnome-keyring",
                "keychain access",
                "credentialuibroker",
            ]
            .map(String::from)
            .to_vec(),
            blocked_titles: ["Private Browsing", "Incognito", "InPrivate", "password"].map(String::from).to_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyCfg {
    /// Linux: bound through a GNOME custom shortcut by `fane setup-shortcut` (GNOME syntax).
    /// macOS/Windows: registered by Fane itself while it runs, e.g. "Ctrl+Alt+Space".
    pub binding: String,
}

impl Default for HotkeyCfg {
    fn default() -> Self {
        let binding = if cfg!(target_os = "linux") {
            "<Control><Super>space"
        } else if cfg!(target_os = "macos") {
            "Cmd+Shift+Space"
        } else {
            "Ctrl+Alt+Space"
        };
        Self { binding: binding.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InsertCfg {
    /// Restore the previous clipboard text after pasting.
    pub restore_clipboard: bool,
    pub paste_delay_ms: u64,
}

impl Default for InsertCfg {
    fn default() -> Self {
        Self { restore_clipboard: true, paste_delay_ms: 180 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectorsCfg {
    pub notes_dirs: Vec<String>,
    pub thunderbird_profiles: Vec<String>,
}

impl Default for ConnectorsCfg {
    fn default() -> Self {
        let thunderbird: &[&str] = if cfg!(target_os = "macos") {
            &["~/Library/Thunderbird"]
        } else if cfg!(target_os = "windows") {
            &["$APPDATA/Thunderbird"]
        } else {
            &["~/.thunderbird", "~/snap/thunderbird/common/.thunderbird"]
        };
        Self {
            notes_dirs: ["~/Notes", "~/Documents/Notes", "~/Obsidian"].map(String::from).to_vec(),
            thunderbird_profiles: thunderbird.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegateCfg {
    pub claude_cmd: Vec<String>,
    pub codex_cmd: Vec<String>,
    pub timeout: u64,
    pub cwd: String,
}

impl Default for DelegateCfg {
    fn default() -> Self {
        Self {
            claude_cmd: vec!["claude".into(), "-p".into()],
            codex_cmd: vec!["codex".into(), "exec".into()],
            timeout: 900,
            cwd: "~".into(),
        }
    }
}

/// Fane's own email: a Google account from GNOME Settings → Online Accounts (Linux only).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentMailCfg {
    /// The one account Fane may use. Empty = email off. Fane never falls back to another.
    pub account: String,
    pub poll_seconds: u64,
    /// Your personal email address. Only mail from it that passes Google's DMARC check is
    /// read, acted on and answered; everything else is ignored and never shown to the model.
    pub owner_email: String,
    /// Older configs listed trusted senders; the first one is used if `owner_email` is empty.
    #[serde(skip_serializing)]
    pub trusted_senders: Vec<String>,
    pub max_auto_replies_per_hour: usize,
    /// Let remote (emailed) requests look at the screen. Off: the reply could leak it.
    pub remote_can_see_screen: bool,
    /// Tools withheld from emailed requests (nobody is at the desk to watch).
    pub remote_exclude_tools: Vec<String>,
    pub signature: String,
}

impl AgentMailCfg {
    /// The one address whose mail Fane acts on (lowercased), if set.
    pub fn owner(&self) -> Option<String> {
        let o = self.owner_email.trim();
        let o = if o.is_empty() { self.trusted_senders.first().map(|s| s.trim()).unwrap_or("") } else { o };
        (!o.is_empty()).then(|| o.to_lowercase())
    }
}

impl Default for AgentMailCfg {
    fn default() -> Self {
        Self {
            account: String::new(),
            poll_seconds: 60,
            owner_email: String::new(),
            trusted_senders: vec![],
            max_auto_replies_per_hour: 20,
            remote_can_see_screen: false,
            remote_exclude_tools: ["insert_text", "insert_image", "delegate", "fane_send_email", "fane_reply_email"]
                .map(String::from)
                .to_vec(),
            signature: "— Fane (assistant)".into(),
        }
    }
}

/// MCP servers (stdio) whose tools are offered to the model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpCfg {
    pub servers: BTreeMap<String, McpServerCfg>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerCfg {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

pub fn default_template() -> String {
    let d = Config::default();
    format!(
        r#"# Fane configuration. Anything omitted falls back to the built-in defaults
# (see src/config.rs in the Fane repository for the full list).

[model]
# Recommended: "openbmb/minicpm-v4.7:35b-a3b", "gemma4:31b", "qwen3.8:27b".
# Compare them on this machine with `fane eval`.
name = "{name}"
# suggest_name = ""   # optional separate model for suggestions
host = "{host}"

[suggestions]
enabled = true
interval_seconds = 25
min_confidence = 0.65
corner = "{corner}"

[privacy]
blocked_apps = ["keepassxc", "1password", "bitwarden", "seahorse"]
blocked_titles = ["Private Browsing", "Incognito", "password"]

[hotkey]
binding = "{binding}"

[connectors]
notes_dirs = ["~/Notes", "~/Documents/Notes"]

[agent_mail]
# account = "my-assistant@gmail.com"   # must be in GNOME Settings → Online Accounts
# owner_email = "me@example.com"       # only mail from you is read, acted on and answered

# Expose MCP server tools to the assistant:
# [mcp.servers.filesystem]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/Documents"]
"#,
        name = d.model.name,
        host = d.model.host,
        corner = d.suggestions.corner,
        binding = d.hotkey.binding,
    )
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&CONFIG_PATH)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, default_template())?;
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// Set one key in the user's config.toml, keeping their comments and layout.
pub fn set_value(section: &str, key: &str, value: impl Into<toml_edit::Value>) -> Result<()> {
    set_value_in(&CONFIG_PATH, section, key, value)
}

pub fn set_value_in(path: &Path, section: &str, key: &str, value: impl Into<toml_edit::Value>) -> Result<()> {
    if !path.exists() {
        Config::load_from(path)?;
    }
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("parsing {}", path.display()))?;
    if !doc.contains_table(section) {
        doc[section] = toml_edit::table();
    }
    let table = doc[section].as_table_mut().with_context(|| format!("[{section}] is not a table"))?;
    match table.get_mut(key).and_then(|item| item.as_value_mut()) {
        Some(existing) => {
            // Keep any comment trailing the old value.
            let decor = existing.decor().clone();
            *existing = value.into();
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::value(value));
        }
    }
    let out = doc.to_string();
    toml::from_str::<Config>(&out).context("refusing to write a config that can't be read back")?;
    std::fs::write(path, out)?;
    Ok(())
}

/// A string list as a TOML array value.
#[cfg(test)]
pub fn str_array(items: &[String]) -> toml_edit::Value {
    let mut a = toml_edit::Array::new();
    for s in items {
        a.push(s.as_str());
    }
    toml_edit::Value::Array(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        let cfg: Config = toml::from_str(&default_template()).unwrap();
        assert_eq!(cfg.model.name, RECOMMENDED_MODELS[0]);
        assert_eq!(cfg.suggestions.interval_seconds, 25);
        assert!(cfg.mcp.servers.is_empty());
        assert_eq!(cfg.agent_mail.max_auto_replies_per_hour, 20);
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let cfg: Config =
            toml::from_str("[model]\nname = \"gemma4:31b\"\n[mcp.servers.fs]\ncommand = \"npx\"\n").unwrap();
        assert_eq!(cfg.model.name, "gemma4:31b");
        assert_eq!(cfg.model.num_ctx, 8192);
        assert_eq!(cfg.mcp.servers["fs"].command, "npx");
    }

    #[test]
    fn set_value_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("fane-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.toml");
        std::fs::write(&p, "# my notes\n[agent_mail]\n# account = \"old\"\npoll_seconds = 5\n").unwrap();
        set_value_in(&p, "agent_mail", "account", "fane@gmail.com").unwrap();
        set_value_in(&p, "agent_mail", "owner_email", "Me@gmail.com").unwrap();
        set_value_in(&p, "agent_mail", "remote_exclude_tools", str_array(&["delegate".into()])).unwrap();
        set_value_in(&p, "agent_mail", "poll_seconds", 9i64).unwrap();
        set_value_in(&p, "model", "host", "http://h:1").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my notes") && text.contains("# account = \"old\""));
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg.agent_mail.account, "fane@gmail.com");
        assert_eq!(cfg.agent_mail.owner().as_deref(), Some("me@gmail.com"));
        assert_eq!(cfg.agent_mail.remote_exclude_tools, vec!["delegate"]);
        assert_eq!(cfg.agent_mail.poll_seconds, 9);
        assert_eq!(cfg.model.host, "http://h:1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_trusted_senders_become_owner() {
        let cfg: Config = toml::from_str("[agent_mail]\ntrusted_senders = [\"Old@x.com\"]\n").unwrap();
        assert_eq!(cfg.agent_mail.owner().as_deref(), Some("old@x.com"));
        assert_eq!(AgentMailCfg::default().owner(), None);
    }

    #[test]
    fn expand_home_and_vars() {
        unsafe { std::env::set_var("FANE_TEST_VAR", "xyz") };
        assert!(expand("~/Notes").ends_with("Notes"));
        assert!(!expand("~/Notes").starts_with("~"));
        assert_eq!(expand("/a/$FANE_TEST_VAR/b"), PathBuf::from("/a/xyz/b"));
    }
}
