//! The coding-harness part of `fane eval`: give a model's paired harness (CodeGem, Qwen Code,
//! SuperQode's DeepSeek harness, ...) a small project with a failing test, then check the test ourselves.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{Config, PAIRINGS};
use crate::connectors::delegate::{Delegate, Vars};
use crate::llm::find_installed;

const HANDLERS: &str = r#"def get_profile(request, db):
    """Return the profile for the logged-in user."""
    session = request["session"]
    user = db[session["uid"]]
    return {
        "id": request["json"]["user_id"],
        "name": user["name"],
    }
"#;

const TEST: &str = r#"import unittest

from handlers import get_profile


class ProfileTest(unittest.TestCase):
    def test_get_profile(self):
        request = {"session": {"uid": 7}, "json": {}}
        db = {7: {"name": "Dana"}}
        self.assertEqual(get_profile(request, db), {"id": 7, "name": "Dana"})


if __name__ == "__main__":
    unittest.main()
"#;

pub const TASK: &str = "The project in the current directory has a failing test: `python3 -m unittest` \
fails in test_get_profile with KeyError: 'user_id' (handlers.py line 6). The profile id must come from \
the logged-in session, not the request body. Fix handlers.py so the test passes. Don't change \
test_handlers.py.";

#[derive(Debug, Clone, Default, Serialize)]
pub struct HarnessRecord {
    pub harness: String,
    pub model: String,
    /// The harness's command is installed.
    pub available: bool,
    /// `python3 -m unittest` passes afterwards.
    pub fixed: bool,
    /// test_handlers.py is byte-for-byte unchanged.
    pub test_untouched: bool,
    /// The reply names the fix (session uid), for harnesses that only answer.
    pub explained: bool,
    pub latency_s: f64,
    /// The frontier model that planned and reviewed, if `--advisor` was used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisor: Option<String>,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HarnessRecord {
    pub fn passed(&self) -> bool {
        self.fixed && self.test_untouched
    }
}

/// The harness paired with `model`'s family (see `config::PAIRINGS`; any tag, so
/// `qwen3.8:latest` counts as Qwen 3.8), else the configured default.
pub fn paired_harness(model: &str, cfg: &Config) -> String {
    PAIRINGS
        .iter()
        .find(|(m, _)| find_installed(m.split(':').next().unwrap_or(m), &[model.to_string()]).is_some())
        .map(|(_, h)| h.to_string())
        .unwrap_or_else(|| cfg.delegate.default.clone())
}

/// Harness commands for the eval: same as configured, but allowed to edit files without asking
/// (the project is a throwaway temp dir), since nobody is there to approve.
fn eval_cfg(cfg: &Config) -> crate::config::DelegateCfg {
    let mut d = cfg.delegate.clone();
    for (name, mut agent) in d.all_agents() {
        if name == "qwen" && !agent.cmd.iter().any(|a| a == "--approval-mode") {
            let at = agent.cmd.iter().position(|a| a == "{task}").unwrap_or(agent.cmd.len());
            agent.cmd.splice(at..at, ["--approval-mode".to_string(), "auto-edit".to_string()]);
            d.agents.insert(name, agent);
        }
    }
    d
}

fn write_project(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("handlers.py"), HANDLERS)?;
    std::fs::write(dir.join("test_handlers.py"), TEST)
}

async fn tests_pass(dir: &Path) -> bool {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let run = tokio::process::Command::new(python)
        .args(["-m", "unittest", "-q"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(tokio::time::timeout(Duration::from_secs(60), run).await, Ok(Ok(s)) if s.success())
}

/// Score a harness's (or anything's) work on the project in `dir`.
pub async fn score(dir: &Path, output: &str, record: &mut HarnessRecord) {
    record.fixed = tests_pass(dir).await;
    record.test_untouched = std::fs::read_to_string(dir.join("test_handlers.py")).is_ok_and(|t| t == TEST);
    let low = output.to_lowercase();
    record.explained = low.contains("session") && low.contains("uid");
}

/// Run `harness` on `model` (an installed Ollama tag) against a fresh copy of the project.
pub async fn run_task(
    cfg: &Config,
    harness: &str,
    model: &str,
    host: &str,
    timeout: Duration,
    with_advisor: bool,
) -> HarnessRecord {
    let mut record = HarnessRecord { harness: harness.into(), model: model.into(), ..Default::default() };
    let mut dcfg = eval_cfg(cfg);
    dcfg.timeout = timeout.as_secs().max(1);
    let delegate = Delegate::new(dcfg);
    record.available = delegate.available().iter().any(|(n, _)| n == harness);
    if !record.available {
        record.error = Some(format!("{harness} is not installed (see `fane doctor`)"));
        return record;
    }
    let dir = std::env::temp_dir().join(format!("fane-harness-{harness}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = write_project(&dir) {
        record.error = Some(format!("cannot create the test project: {e}"));
        return record;
    }
    let vars = Vars { model: model.into(), ollama_host: host.into() };
    let t0 = Instant::now();
    let cwd = dir.display().to_string();
    let output = match crate::connectors::review::advisor(&delegate.cfg).filter(|_| with_advisor) {
        Some(adv) => {
            record.advisor = Some(adv.0.clone());
            let progress = |s: &str| println!("    {s}");
            delegate.run_with_advisor(Some(adv), harness, TASK, &cwd, &vars, &progress).await
        }
        None => delegate.run(harness, TASK, &cwd, &vars).await,
    };
    record.latency_s = (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    if output.starts_with("error:") {
        record.error = Some(crate::llm::truncate(&output, 300).to_string());
    }
    score(&dir, &output, &mut record).await;
    record.output = crate::llm::truncate(&output, 4000).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("fane-harness-test-{name}-{}", std::process::id()))
    }

    #[test]
    fn pairings() {
        let cfg = Config::default();
        assert_eq!(paired_harness("gemma4:26b", &cfg), "codegem");
        assert_eq!(paired_harness("qwen3.8:27b", &cfg), "qwen");
        assert_eq!(paired_harness("qwen3.8:latest", &cfg), "qwen");
        assert_eq!(paired_harness("openbmb/minicpm-v4.7:35b-a3b", &cfg), "deepseek");
        assert_eq!(paired_harness("llava:13b", &cfg), "codegem");
    }

    #[test]
    fn eval_lets_qwen_edit() {
        let d = eval_cfg(&Config::default());
        let qwen = d.all_agents().into_iter().find(|(n, _)| n == "qwen").unwrap().1;
        let i = qwen.cmd.iter().position(|a| a == "--approval-mode").unwrap();
        assert_eq!(qwen.cmd[i + 1], "auto-edit");
        assert_eq!(qwen.cmd.last().unwrap(), "{task}");
    }

    #[tokio::test]
    async fn scoring_detects_a_real_fix() {
        if which::which(if cfg!(windows) { "python" } else { "python3" }).is_err() {
            return;
        }
        let dir = tmp("score");
        write_project(&dir).unwrap();
        let mut broken = HarnessRecord::default();
        score(&dir, "no idea", &mut broken).await;
        assert!(!broken.fixed && broken.test_untouched && !broken.explained);

        std::fs::write(dir.join("handlers.py"), HANDLERS.replace(r#"request["json"]["user_id"]"#, r#"session["uid"]"#))
            .unwrap();
        let mut good = HarnessRecord::default();
        score(&dir, "Use session['uid'] instead.", &mut good).await;
        assert!(good.passed() && good.explained, "{good:?}");

        std::fs::write(dir.join("test_handlers.py"), "import unittest\n").unwrap();
        let mut cheat = HarnessRecord::default();
        score(&dir, "", &mut cheat).await;
        assert!(!cheat.test_untouched && !cheat.passed());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
