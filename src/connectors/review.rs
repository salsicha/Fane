//! Frontier-model planning and review around local coding harnesses.
//!
//! When Claude Code (or Codex) is installed, a coding task handed to a local harness runs as:
//! the frontier model plans it (read-only), the local harness implements the plan, the
//! frontier model reviews exactly what changed, and the harness fixes what the review finds,
//! for up to `review_rounds` rounds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

use crate::config::{AgentCfg, DelegateCfg};
use crate::connectors::delegate::{Delegate, Vars, exec_status};

/// Harnesses that are frontier models themselves (no advisor around them).
pub const FRONTIER: [&str; 2] = ["claude", "codex"];

/// Largest diff sent for review; beyond this only the start is shown.
const MAX_DIFF: usize = 60_000;
/// Folders bigger than this (outside a git repository) aren't snapshotted for review.
const MAX_FILES: usize = 5_000;

/// The advisor's read-only command: it can read the project but not change it.
fn advisor_cmd(name: &str) -> Option<AgentCfg> {
    let cmd: &[&str] = match name {
        "claude" => &["claude", "-p", "--permission-mode", "plan", "{task}"],
        "codex" => &["codex", "exec", "--skip-git-repo-check", "--sandbox", "read-only", "{task}"],
        _ => return None,
    };
    Some(AgentCfg { cmd: cmd.iter().map(|s| s.to_string()).collect(), ..Default::default() })
}

pub fn advisor_label(name: &str) -> &'static str {
    if name == "codex" { "Codex" } else { "Claude" }
}

/// The installed advisor per `[delegate] advisor`, if any.
pub fn advisor(cfg: &DelegateCfg) -> Option<(String, AgentCfg)> {
    let wanted: &[&str] = match cfg.advisor.trim().to_lowercase().as_str() {
        "off" | "none" | "" => return None,
        "claude" => &["claude"],
        "codex" => &["codex"],
        _ => &["claude", "codex"],
    };
    wanted.iter().find_map(|n| {
        let spec = advisor_cmd(n)?;
        spec.cmd.first().is_some_and(|c| which::which(c).is_ok()).then(|| (n.to_string(), spec))
    })
}

fn plan_prompt(task: &str, dir: &Path) -> String {
    format!(
        "You are planning a coding task that a smaller local model will carry out in {dir}. \
Read the relevant code (you can't change anything), then write a concise, concrete plan: which \
files to change and how, edge cases to handle, and how to verify the result (which tests or \
commands to run). Don't write out the full code.\n\nTask:\n{task}",
        dir = dir.display()
    )
}

fn review_prompt(task: &str, plan: &str, diff: &str, dir: &Path) -> String {
    format!(
        "You are reviewing changes a smaller local model just made in {dir}. You can read the \
project but not change it.\n\nTask:\n{task}\n\nPlan it was given:\n{plan}\n\nIts changes:\n```diff\n{diff}\n```\n\n\
Check for bugs, missed requirements, broken or missing tests, edits outside the task, and anything \
that won't work. If the changes are correct and complete, reply with exactly LGTM on the first line \
(optionally followed by one short note). Otherwise list each problem as a numbered item naming the \
file and the concrete fix. Be specific and don't restate the diff.",
        dir = dir.display()
    )
}

/// True if a review approves the changes.
pub fn approved(review: &str) -> bool {
    review.lines().map(str::trim).find(|l| !l.is_empty()).is_some_and(|l| {
        let l = l.trim_start_matches(['*', '#', '`', ' ']).to_uppercase();
        l.starts_with("LGTM")
    })
}

// --- snapshots of the working folder, to diff exactly what the harness changed ----------

/// A git tree of the folder's files at one moment, written to a private index so the user's
/// own git index and working tree are never touched.
struct Snapshots {
    /// `git` arguments that select the repository (and work tree, for plain folders).
    git: Vec<String>,
    root: PathBuf,
    index: PathBuf,
    _tmp: TempDir,
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn git(args: &[String], root: &Path, index: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(root).stdin(std::process::Stdio::null()).kill_on_drop(true);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let out = tokio::time::timeout(Duration::from_secs(120), cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

impl Snapshots {
    async fn new(dir: &Path) -> Option<Self> {
        which::which("git").ok()?;
        let tmp = TempDir(std::env::temp_dir().join(format!("fane-review-{}-{}", std::process::id(), rand_suffix())));
        std::fs::create_dir_all(&tmp.0).ok()?;
        let index = tmp.0.join("index");
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        if let Some(top) = git(&s(&["rev-parse", "--show-toplevel"]), dir, None).await {
            // Inside a repository: use it (honours .gitignore), with our own index seeded from HEAD.
            let root = PathBuf::from(top);
            let _ = git(&s(&["read-tree", "HEAD"]), &root, Some(&index)).await;
            return Some(Self { git: vec![], root, index, _tmp: tmp });
        }
        // A plain folder: a throwaway repository beside it, if the folder is small enough.
        if dir == dirs::home_dir()?.as_path()
            || walkdir::WalkDir::new(dir).into_iter().take(MAX_FILES + 1).count() > MAX_FILES
        {
            return None;
        }
        let git_dir = tmp.0.join("repo");
        git(&s(&["init", "-q", "--bare", &git_dir.display().to_string()]), &tmp.0, None).await?;
        let args = vec![format!("--git-dir={}", git_dir.display()), format!("--work-tree={}", dir.display())];
        Some(Self { git: args, root: dir.to_path_buf(), index, _tmp: tmp })
    }

    fn args(&self, rest: &[&str]) -> Vec<String> {
        self.git.iter().cloned().chain(rest.iter().map(|s| s.to_string())).collect()
    }

    /// Tree id of the folder as it is now.
    async fn take(&self) -> Option<String> {
        git(&self.args(&["add", "-A", "."]), &self.root, Some(&self.index)).await?;
        git(&self.args(&["write-tree"]), &self.root, Some(&self.index)).await
    }

    async fn diff(&self, before: &str, after: &str) -> String {
        git(&self.args(&["diff", "--no-color", "--find-renames", before, after]), &self.root, None)
            .await
            .unwrap_or_default()
    }
}

fn rand_suffix() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0)
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let cut = (0..=max).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    format!("{}\n... (diff truncated; {} more bytes)", &text[..cut], text.len() - cut)
}

impl Delegate {
    /// Run `agent` on `task`, with planning and review by the frontier advisor when one is
    /// installed and `agent` is a local harness. `progress` gets short status lines.
    pub async fn run_reviewed(
        &self,
        agent: &str,
        task: &str,
        cwd: &str,
        vars: &Vars,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> String {
        self.run_with_advisor(advisor(&self.cfg), agent, task, cwd, vars, progress).await
    }

    /// `run_reviewed` with a given advisor command (tests use a fake one).
    pub async fn run_with_advisor(
        &self,
        advisor: Option<(String, AgentCfg)>,
        agent: &str,
        task: &str,
        cwd: &str,
        vars: &Vars,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> String {
        let agent = agent.trim().to_lowercase();
        let advisor = advisor.filter(|_| !FRONTIER.contains(&agent.as_str()));
        let Some((advisor_name, advisor_spec)) = advisor else {
            return self.run(&agent, task, cwd, vars).await;
        };
        if self.agent(&agent).is_none() {
            return self.run(&agent, task, cwd, vars).await; // reports the unknown agent
        }
        let who = advisor_label(&advisor_name);
        let dir = self.workdir(cwd);
        let timeout = Duration::from_secs(self.cfg.timeout);
        let ask = |prompt: String| {
            let spec = advisor_spec.clone();
            let dir = dir.clone();
            let label = advisor_name.clone();
            async move { exec_status(&label, &spec, &prompt, &dir, vars, timeout).await }
        };
        let mut report = vec![];

        progress(&format!("Asking {who} for a plan…"));
        let (plan, ok) = ask(plan_prompt(task, &dir)).await;
        let plan = if ok {
            Some(plan)
        } else {
            let why = crate::llm::truncate(&plan, 300).to_string();
            report.push(format!("{who} couldn't plan this ({why}); {agent} worked without a plan or review."));
            None
        };

        let snaps = if plan.is_some() { Snapshots::new(&dir).await } else { None };
        let before = match &snaps {
            Some(s) => s.take().await,
            None => None,
        };
        let implement = match &plan {
            Some(p) => format!("{task}\n\nPlan from a senior reviewer (follow it unless it's clearly wrong):\n{p}"),
            None => task.to_string(),
        };
        progress(&format!("{agent} is working…"));
        let mut output = self.run(&agent, &implement, cwd, vars).await;
        let Some(plan) = plan else {
            report.push(format!("{agent}:\n{output}"));
            return report.join("\n\n");
        };
        report.push(format!("Plan ({who}):\n{plan}"));

        let (Some(snaps), Some(before)) = (snaps, before) else {
            report.push(format!("{agent}:\n{output}"));
            report.push(format!("Not reviewed: {} isn't a git repository and is too big to snapshot.", dir.display()));
            return report.join("\n\n");
        };
        let mut verdict = String::new();
        for round in 1..=self.cfg.review_rounds.max(1) {
            let Some(after) = snaps.take().await else { break };
            let diff = snaps.diff(&before, &after).await;
            if diff.trim().is_empty() {
                verdict = format!("Not reviewed: {agent} didn't change any files.");
                break;
            }
            progress(&format!("Asking {who} to review the changes…"));
            let (review, ok) = ask(review_prompt(task, &plan, &clip(&diff, MAX_DIFF), &dir)).await;
            if !ok {
                verdict = format!("{who}'s review failed: {}", crate::llm::truncate(&review, 300));
                break;
            }
            if approved(&review) {
                verdict = format!("Review ({who}, round {round}): approved. {}", review.trim());
                break;
            }
            if round == self.cfg.review_rounds.max(1) {
                verdict = format!("Review ({who}, round {round}) still found problems:\n{review}");
                break;
            }
            progress(&format!("{agent} is fixing what {who} found…"));
            let fix = format!(
                "{task}\n\nYou already made changes for this. A senior reviewer found these problems; \
                 fix them all:\n{review}"
            );
            output = self.run(&agent, &fix, cwd, vars).await;
        }
        report.push(format!("{agent}:\n{output}"));
        report.push(verdict);
        report.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts() {
        assert!(approved("LGTM"));
        assert!(approved("\n**LGTM** — tests pass"));
        assert!(approved("lgtm, nice"));
        assert!(!approved("1. handlers.py: still reads user_id\nLGTM otherwise"));
        assert!(!approved(""));
    }

    #[test]
    fn advisor_selection() {
        let cfg = |a: &str| DelegateCfg { advisor: a.into(), ..Default::default() };
        assert!(advisor(&cfg("off")).is_none());
        if let Some((name, spec)) = advisor(&cfg("auto")) {
            assert!(FRONTIER.contains(&name.as_str()));
            assert!(spec.cmd.iter().any(|a| a == "plan" || a == "read-only"), "advisor must be read-only");
        }
    }

    #[test]
    fn clip_keeps_char_boundaries() {
        let s = "é".repeat(10);
        assert!(clip(&s, 5).starts_with("éé"));
        assert_eq!(clip("short", 100), "short");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshots_capture_only_the_new_changes() {
        if which::which("git").is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("fane-review-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "x = 1\n").unwrap();
        let snaps = Snapshots::new(&dir).await.expect("small plain folder is snapshotted");
        let before = snaps.take().await.unwrap();
        std::fs::write(dir.join("a.py"), "x = 2\n").unwrap();
        std::fs::write(dir.join("b.py"), "new\n").unwrap();
        let after = snaps.take().await.unwrap();
        let diff = snaps.diff(&before, &after).await;
        assert!(diff.contains("-x = 1") && diff.contains("+x = 2") && diff.contains("b.py"), "{diff}");
        assert!(!dir.join(".git").exists(), "the user's folder must not get a .git");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fake advisor and harness (shell scripts) exercise the whole plan → work → review →
    /// fix loop without a model.
    #[cfg(unix)]
    #[tokio::test]
    async fn plan_work_review_fix_loop() {
        if which::which("git").is_err() || which::which("sh").is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("fane-review-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "old\n").unwrap();
        // The "harness" writes "draft" first, then "fixed" when asked to fix problems.
        let harness = AgentCfg {
            cmd: vec![
                "sh".into(),
                "-c".into(),
                r#"case "$1" in *"fix them all"*) echo fixed > f.txt;; *) echo draft > f.txt;; esac; echo done"#.into(),
                "x".into(),
                "{task}".into(),
            ],
            ..Default::default()
        };
        let mut cfg =
            DelegateCfg { cwd: dir.display().to_string(), timeout: 30, review_rounds: 2, ..Default::default() };
        cfg.agents.insert("codegem".into(), harness);
        let delegate = Delegate::new(cfg);
        // The "advisor": plans, rejects "draft", approves "fixed" (it sees the diff in its prompt).
        let advisor = AgentCfg {
            cmd: vec![
                "sh".into(),
                "-c".into(),
                r#"case "$1" in *"+fixed"*) echo LGTM;; *"+draft"*) echo "1. f.txt: say fixed";; *) echo "Plan: edit f.txt";; esac"#.into(),
                "x".into(),
                "{task}".into(),
            ],
            ..Default::default()
        };
        let steps = std::sync::Mutex::new(vec![]);
        let progress = |p: &str| steps.lock().unwrap().push(p.to_string());
        let report = delegate
            .run_with_advisor(
                Some(("claude".into(), advisor)),
                "codegem",
                "change f.txt",
                "",
                &Vars::default(),
                &progress,
            )
            .await;
        assert!(report.contains("Plan (Claude):\nPlan: edit f.txt"), "{report}");
        assert!(report.contains("round 2): approved"), "{report}");
        let steps = steps.lock().unwrap().clone();
        assert_eq!(
            steps,
            [
                "Asking Claude for a plan…",
                "codegem is working…",
                "Asking Claude to review the changes…",
                "codegem is fixing what Claude found…",
                "Asking Claude to review the changes…"
            ]
        );
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "fixed\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
