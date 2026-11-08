//! Hand hard tasks to Claude Code or Codex running non-interactively.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::config::{DelegateCfg, expand};

const AGENTS: [&str; 2] = ["claude", "codex"];

pub struct Delegate {
    pub cfg: DelegateCfg,
}

impl Delegate {
    pub fn new(cfg: DelegateCfg) -> Self {
        Self { cfg }
    }

    fn cmd(&self, agent: &str) -> &[String] {
        match agent {
            "claude" => &self.cfg.claude_cmd,
            _ => &self.cfg.codex_cmd,
        }
    }

    /// "claude" / "codex" whose command is on PATH.
    pub fn available(&self) -> Vec<String> {
        AGENTS
            .iter()
            .filter(|a| self.cmd(a).first().is_some_and(|c| which::which(c).is_ok()))
            .map(|a| a.to_string())
            .collect()
    }

    /// Runs the agent with `task` as the last argument; returns its (trimmed) output or "error: ...".
    pub async fn run(&self, agent: &str, task: &str, cwd: &str) -> String {
        let agent = agent.trim().to_lowercase();
        if !AGENTS.contains(&agent.as_str()) {
            return "error: agent must be 'claude' or 'codex'".into();
        }
        let Some((program, args)) = self.cmd(&agent).split_first() else {
            return format!("error: no command configured for {agent}");
        };
        let Ok(program_path) = which::which(program) else {
            return format!("error: {program} is not installed");
        };
        let workdir = expand(if cwd.trim().is_empty() { &self.cfg.cwd } else { cwd });
        let mut child = match Command::new(program_path)
            .args(args)
            .arg(task)
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return format!("error: could not start {agent} in {}: {e}", workdir.display()),
        };

        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return format!("error: could not capture {agent} output");
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
        let out = match tokio::time::timeout(Duration::from_secs(self.cfg.timeout), collect).await {
            Ok(out) => out,
            Err(_) => {
                let _ = child.kill().await;
                return format!("error: {agent} timed out after {}s", self.cfg.timeout);
            }
        };
        let code = match child.wait().await {
            Ok(status) => status.code().map_or_else(|| "signal".to_string(), |c| c.to_string()),
            Err(_) => "unknown".into(),
        };
        let text = clip_output(String::from_utf8_lossy(&out).trim());
        if text.is_empty() { format!("{agent} finished with no output (exit {code})") } else { text }
    }
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

    fn cfg(claude: &[&str], timeout: u64) -> DelegateCfg {
        DelegateCfg {
            claude_cmd: claude.iter().map(|s| s.to_string()).collect(),
            codex_cmd: vec!["fane-no-such-command-xyz".into()],
            timeout,
            cwd: std::env::temp_dir().display().to_string(),
        }
    }

    #[test]
    fn available_skips_missing() {
        let d = Delegate::new(cfg(&["fane-no-such-command-abc"], 5));
        assert!(d.available().is_empty());
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
        assert_eq!(d.run("gpt", "x", "").await, "error: agent must be 'claude' or 'codex'");
        assert_eq!(d.run("codex", "x", "").await, "error: fane-no-such-command-xyz is not installed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_agent() {
        let d = Delegate::new(cfg(&["sh", "-c", "echo hi; echo \"$0\" >&2; pwd"], 5));
        assert_eq!(d.available(), vec!["claude"]);
        let out = d.run(" Claude ", "do the thing", "/").await;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines.contains(&"hi") && lines.contains(&"do the thing") && lines.contains(&"/"));

        let quiet = Delegate::new(cfg(&["sh", "-c", "exit 3"], 5));
        assert_eq!(quiet.run("claude", "t", "").await, "claude finished with no output (exit 3)");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn times_out() {
        let d = Delegate::new(cfg(&["sh", "-c", "sleep 30"], 1));
        let start = std::time::Instant::now();
        assert_eq!(d.run("claude", "t", "").await, "error: claude timed out after 1s");
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
