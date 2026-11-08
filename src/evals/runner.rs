//! Runs the eval suite and writes the report.
// STUB: replaced by the evals port.

use crate::config::Config;

#[derive(Debug, Clone, Default)]
pub struct EvalOptions {
    pub models: Vec<String>,
    pub repeat: usize,
    pub only: Option<String>,
    pub filter: Option<String>,
    pub timeout_s: f64,
    pub out: Option<std::path::PathBuf>,
}

/// Print the cases (`fane eval --list`).
pub fn list_cases() -> Vec<String> {
    vec![]
}

/// Run the suite, print progress and the report, save results; returns the exit code.
pub async fn run(_cfg: Config, _opts: EvalOptions) -> anyhow::Result<i32> {
    anyhow::bail!("not implemented")
}
