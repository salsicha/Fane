//! Hand hard tasks to Claude Code or Codex running non-interactively.
// STUB: replaced by the connectors port.

use crate::config::DelegateCfg;

pub struct Delegate {
    pub cfg: DelegateCfg,
}

impl Delegate {
    pub fn new(cfg: DelegateCfg) -> Self {
        Self { cfg }
    }

    /// "claude" / "codex" whose command is on PATH.
    pub fn available(&self) -> Vec<String> {
        vec![]
    }

    /// Runs the agent with `task` as the last argument; returns its (trimmed) output or "error: ...".
    pub async fn run(&self, _agent: &str, _task: &str, _cwd: &str) -> String {
        "error: not implemented".into()
    }
}
