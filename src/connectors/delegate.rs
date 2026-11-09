//! Hand hard tasks to a command-line coding agent (Qwen Code, CodeGem, Claude Code, Codex, or
//! one you configure) running non-interactively.

use std::process::Stdio;
use std::sync::RwLock;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::config::{AgentCfg, DelegateCfg, expand};

/// Fane's models and model server, for the `{model}`, `{code_model}`, `{base_url}` and
/// `{ollama_host}` placeholders.
#[derive(Debug, Clone, Default)]
pub struct Vars {
    pub model: String,
    pub code_model: String,
    /// The model server's address without `/v1`.
    pub host: String,
}

pub struct Delegate {
    pub cfg: DelegateCfg,
    pub(crate) agents: Vec<(String, AgentCfg)>,
    /// Preference order when `default` is "auto" (set from `[routes] code`).
    order: RwLock<Vec<String>>,
}

/// Fill in the placeholders.
fn fill(template: &str, task: &str, model: &str, code_model: &str, host: &str, cwd: &str) -> String {
    template
        .replace("{model}", model)
        .replace("{code_model}", code_model)
        .replace("{base_url}", &format!("{host}/v1"))
        .replace("{ollama_host}", host)
        .replace("{cwd}", cwd)
        .replace("{task}", task)
}

impl Delegate {
    pub fn new(cfg: DelegateCfg) -> Self {
        let agents = cfg.all_agents();
        Self { cfg, agents, order: RwLock::new(vec![]) }
    }

    /// Which harnesses to prefer, best first (used when `default` is "auto").
    pub fn set_order(&self, order: Vec<String>) {
        *self.order.write().unwrap() = order;
    }

    pub(crate) fn agent(&self, name: &str) -> Option<&AgentCfg> {
        self.agents.iter().find(|(n, _)| n == name).map(|(_, a)| a)
    }

    /// (name, description) of the harnesses whose command is installed, preferred first.
    pub fn available(&self) -> Vec<(String, String)> {
        let mut list: Vec<(String, String)> = self
            .agents
            .iter()
            .filter(|(_, a)| a.cmd.first().is_some_and(|c| which::which(c).is_ok()))
            .map(|(n, a)| (n.clone(), a.description.clone()))
            .collect();
        if self.cfg.default == "auto" {
            let order = self.order.read().unwrap();
            let rank = |n: &str| order.iter().position(|o| o == n).unwrap_or(order.len());
            list.sort_by_key(|(n, _)| rank(n)); // stable: built-in order breaks ties
        }
        list
    }

    /// Runs the harness on `task`; returns its (trimmed) output or "error: ...".
    pub async fn run(&self, agent: &str, task: &str, cwd: &str, vars: &Vars) -> String {
        let agent = agent.trim().to_lowercase();
        let Some(spec) = self.agent(&agent) else {
            let names: Vec<&str> = self.agents.iter().map(|(n, _)| n.as_str()).collect();
            return format!("error: unknown agent {agent:?} (configured: {})", names.join(", "));
        };
        exec(&agent, spec, task, &self.workdir(cwd), vars, Duration::from_secs(self.cfg.timeout)).await
    }

    pub fn workdir(&self, cwd: &str) -> std::path::PathBuf {
        expand(if cwd.trim().is_empty() { &self.cfg.cwd } else { cwd })
    }
}

/// Run one command-line agent non-interactively and collect its output (or "error: ...").
pub async fn exec(
    label: &str,
    spec: &AgentCfg,
    task: &str,
    workdir: &std::path::Path,
    vars: &Vars,
    timeout: Duration,
) -> String {
    exec_status(label, spec, task, workdir, vars, timeout).await.0
}

/// Like `exec`, plus whether the command ran and exited successfully.
pub async fn exec_status(
    label: &str,
    spec: &AgentCfg,
    task: &str,
    workdir: &std::path::Path,
    vars: &Vars,
    timeout: Duration,
) -> (String, bool) {
    let Some((program, args)) = spec.cmd.split_first() else {
        return (format!("error: no command configured for {label}"), false);
    };
    let Ok(program_path) = which::which(program) else {
        return (format!("error: {program} is not installed"), false);
    };
    let model = if spec.model.is_empty() { vars.model.as_str() } else { spec.model.as_str() };
    let code_model = if !spec.model.is_empty() {
        spec.model.as_str()
    } else if vars.code_model.is_empty() {
        vars.model.as_str()
    } else {
        vars.code_model.as_str()
    };
    let dir = workdir.display().to_string();
    let fill = |t: &str| fill(t, task, model, code_model, &vars.host, &dir);
    let mut argv: Vec<String> = args.iter().map(|a| fill(a)).collect();
    if !spec.cmd.iter().any(|a| a.contains("{task}")) {
        argv.push(task.to_string());
    }
    let mut cmd = Command::new(program_path);
    cmd.args(&argv);
    for (k, v) in &spec.env {
        cmd.env(k, fill(v));
    }
    let mut child = match cmd
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (format!("error: could not start {label} in {}: {e}", workdir.display()), false),
    };

    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return (format!("error: could not capture {label} output"), false);
    };
    let collect = async {
        // stdout and stderr interleaved in arrival order.
        let mut out = Vec::new();
        let (mut b1, mut b2) = ([0u8; 8192], [0u8; 8192]);
        let (mut done1, mut done2) = (false, false);
        while !(done1 && done2) {
            tokio::select! {
                r = stdout.read(&mut b1), if !done1 => match r {
                    Ok(n) if n > 0 => out.extend_from_slice(&b1[..n]),
                    _ => done1 = true,
                },
                r = stderr.read(&mut b2), if !done2 => match r {
                    Ok(n) if n > 0 => out.extend_from_slice(&b2[..n]),
                    _ => done2 = true,
                },
            }
        }
        out
    };
    let out = match tokio::time::timeout(timeout, collect).await {
        Ok(out) => out,
        Err(_) => {
            let _ = child.kill().await;
            return (format!("error: {label} timed out after {}s", timeout.as_secs()), false);
        }
    };
    let (code, ok) = match child.wait().await {
        Ok(status) => (status.code().map_or_else(|| "signal".to_string(), |c| c.to_string()), status.success()),
        Err(_) => ("unknown".into(), false),
    };
    let text = clip_output(String::from_utf8_lossy(&out).trim());
    let text = if text.is_empty() { format!("{label} finished with no output (exit {code})") } else { text };
    (text, ok)
}

/// Keep long output readable: the first 2000 and last 10000 chars of anything over 12000.
fn clip_output(text: &str) -> String {
    let n = text.chars().count();
    if n <= 12_000 {
        return text.to_string();
    }
    let head: String = text.chars().take(2000).collect();
    let tail: String = text.chars().skip(n - 10_000).collect();
    format!("{head}\n...\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a `claude` harness (with this command) and a missing `codex` one.
    fn cfg(claude: &[&str], timeout: u64) -> DelegateCfg {
        let mut agents = std::collections::BTreeMap::new();
        for name in ["codegem", "qwen"] {
            agents.insert(name.into(), AgentCfg { cmd: vec!["fane-no-such-command-xyz".into()], ..Default::default() });
        }
        DelegateCfg {
            claude_cmd: claude.iter().map(|s| s.to_string()).collect(),
            codex_cmd: vec!["fane-no-such-command-xyz".into()],
            timeout,
            cwd: std::env::temp_dir().display().to_string(),
            agents,
            ..Default::default()
        }
    }

    fn vars() -> Vars {
        Vars { model: "gemma4:26b".into(), code_model: "qwen3.8:27b".into(), host: "http://127.0.0.1:11500".into() }
    }

    #[test]
    fn available_skips_missing() {
        let d = Delegate::new(cfg(&["fane-no-such-command-abc"], 5));
        assert!(d.available().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn auto_order_follows_routes() {
        let mut c = cfg(&["sh"], 5);
        c.codex_cmd = vec!["sh".into()];
        let d = Delegate::new(c);
        let names = |d: &Delegate| d.available().into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names(&d), ["claude", "codex"]);
        d.set_order(vec!["codex".into(), "claude".into()]);
        assert_eq!(names(&d), ["codex", "claude"]);
    }

    #[test]
    fn clip() {
        assert_eq!(clip_output("short"), "short");
        let long: String = "é".repeat(2000) + &"m".repeat(5000) + &"z".repeat(10_000);
        let c = clip_output(&long);
        assert!(c.starts_with(&"é".repeat(2000)));
        assert!(c.ends_with(&format!("\n...\n{}", "z".repeat(10_000))));
        assert_eq!(c.chars().count(), 2000 + 5 + 10_000);
    }

    #[tokio::test]
    async fn rejects_unknown_agent_and_missing_command() {
        let d = Delegate::new(cfg(&["sh"], 5));
        assert!(d.run("gpt", "x", "", &vars()).await.starts_with("error: unknown agent \"gpt\""));
        assert_eq!(d.run("codex", "x", "", &vars()).await, "error: fane-no-such-command-xyz is not installed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_agent() {
        let d = Delegate::new(cfg(&["sh", "-c", "echo hi; echo \"$0\" >&2; pwd"], 5));
        assert_eq!(d.available().iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["claude"]);
        let out = d.run(" Claude ", "do the thing", "/", &vars()).await;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines.contains(&"hi") && lines.contains(&"do the thing") && lines.contains(&"/"));

        let quiet = Delegate::new(cfg(&["sh", "-c", "exit 3"], 5));
        assert_eq!(quiet.run("claude", "t", "", &vars()).await, "claude finished with no output (exit 3)");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn times_out() {
        let d = Delegate::new(cfg(&["sh", "-c", "sleep 30"], 1));
        let start = std::time::Instant::now();
        assert_eq!(d.run("claude", "t", "", &vars()).await, "error: claude timed out after 1s");
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fills_placeholders_and_env() {
        let mut cfg = cfg(&["fane-no-such-command-abc"], 5);
        cfg.agents.insert(
            "codegem".into(),
            AgentCfg {
                cmd: vec![
                    "sh".into(),
                    "-c".into(),
                    "echo \"$1|$2|$3|$LLAMACPP_HOST\"".into(),
                    "x".into(),
                    "{model}".into(),
                    "{code_model}".into(),
                    "{task}".into(),
                ],
                env: [("LLAMACPP_HOST".to_string(), "{base_url}".to_string())].into(),
                ..Default::default()
            },
        );
        let d = Delegate::new(cfg);
        assert_eq!(d.available()[0].0, "codegem");
        assert_eq!(
            d.run("codegem", "fix it", "", &vars()).await,
            "gemma4:26b|qwen3.8:27b|fix it|http://127.0.0.1:11500/v1"
        );
    }
}
