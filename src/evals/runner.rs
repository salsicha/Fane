//! `fane eval`: compare local vision models on Fane's real prompts and tools.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

use crate::agent::{Agent, ToolModes};
use crate::config::{Config, RECOMMENDED_MODELS, STATE_DIR};
use crate::evals::scenarios::{AgentTask, Delegation, Mocks, SUGGEST_CASES, SuggestCase, agent_tasks};
use crate::evals::scenes::{scene, scenes};
use crate::llm::{LlmResult, Ollama, find_installed, truncate};
use crate::snapshot::{Snapshot, WindowInfo};
use crate::suggester::Suggester;

#[derive(Debug, Clone, Default)]
pub struct EvalOptions {
    /// Ollama tags; empty means `config::RECOMMENDED_MODELS`.
    pub models: Vec<String>,
    /// Run each case N times (0 is treated as 1).
    pub repeat: usize,
    /// "suggest" or "agent" to run one half of the suite.
    pub only: Option<String>,
    /// Only cases whose name contains this.
    pub filter: Option<String>,
    /// Seconds per case (0 or less means the default of 600).
    pub timeout_s: f64,
    /// Output directory (default `STATE_DIR/evals/<YYYYmmdd-HHMMSS>`).
    pub out: Option<PathBuf>,
}

// --- Python-compatible number formatting ---------------------------------------------

/// Python's `round(x, nd)` (correctly rounded, ties to even).
fn round_to(x: f64, nd: usize) -> f64 {
    format!("{x:.nd$}").parse().unwrap_or(x)
}

/// Python's `round(x)` -> int.
fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `str(float)`: always shows a decimal point.
fn pyf(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e16 { format!("{x:.1}") } else { format!("{x}") }
}

/// A number that is an int or a float, as Python's `statistics.median` returns.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Num {
    Int(i64),
    Float(f64),
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Num::Int(i) => write!(f, "{i}"),
            Num::Float(x) => write!(f, "{}", pyf(*x)),
        }
    }
}

fn median_f(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let m = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
    Some(round_to(m, 1))
}

fn median_i(xs: &[i64]) -> Option<Num> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort();
    let n = v.len();
    Some(if n % 2 == 1 { Num::Int(v[n / 2]) } else { Num::Float(round_to((v[n / 2 - 1] + v[n / 2]) as f64 / 2.0, 1)) })
}

fn dash<T: fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "–".to_string(), |v| v.to_string())
}

fn dash_f(v: Option<f64>) -> String {
    v.map_or_else(|| "–".to_string(), pyf)
}

/// Python's `str()` of a JSON value pulled out of a dict with `.get()`.
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        Some(v) => v.to_string(),
    }
}

/// Python's `repr()` of a list of strings.
fn py_list_repr(items: &[&str]) -> String {
    let reprs: Vec<String> = items
        .iter()
        .map(|s| {
            let s = s.replace('\\', "\\\\");
            if s.contains('\'') && !s.contains('"') {
                format!("\"{s}\"")
            } else {
                format!("'{}'", s.replace('\'', "\\'"))
            }
        })
        .collect();
    format!("[{}]", reprs.join(", "))
}

/// Python's `float(x or 0)` for a suggestion's confidence; None if it wouldn't parse.
fn py_float(v: Option<&Value>) -> Option<f64> {
    match v {
        None | Some(Value::Null) => Some(0.0),
        Some(Value::Bool(b)) => Some(f64::from(u8::from(*b))),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.is_empty() => Some(0.0),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(_) => None,
    }
}

// --- records ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct SuggestRecord {
    pub case: String,
    pub expect: Option<String>,
    pub json_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub latency_s: f64,
    pub tok_s: Option<f64>,
    pub activity: String,
    pub suggestions: Vec<Value>,
    pub understood: bool,
    pub behaved: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallRecord {
    pub name: String,
    pub args: Value,
}

/// Named check results in task order; serialized as a JSON object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checks(pub Vec<(String, bool)>);

impl Checks {
    pub fn passed(&self) -> usize {
        self.0.iter().filter(|(_, ok)| *ok).count()
    }

    pub fn all(&self) -> bool {
        self.0.iter().all(|(_, ok)| *ok)
    }
}

impl Serialize for Checks {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentRecord {
    pub case: String,
    pub prompt: String,
    pub latency_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_calls: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_eval_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_thinking_chars: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<Vec<CallRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inserted: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegated: Option<Vec<Delegation>>,
    pub checks: Checks,
    pub passed: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub understanding: (usize, usize),
    pub helpful: (usize, usize),
    pub quiet: (usize, usize),
    pub json_ok: (usize, usize),
    pub agent_tasks: (usize, usize),
    pub agent_checks: (usize, usize),
    pub suggest_score: Option<f64>,
    pub quality: Option<f64>,
    pub suggest_latency_s: Option<f64>,
    pub agent_latency_s: Option<f64>,
    pub tok_s: Option<f64>,
    pub tool_calls: Option<Num>,
    pub agent_tokens: Option<Num>,
    pub thinking_chars: Option<Num>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelReport {
    pub model: String,
    /// "ok", "missing" or "error".
    pub status: String,
    pub suggest: Vec<SuggestRecord>,
    pub agent: Vec<AgentRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_pct: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
}

impl ModelReport {
    fn name(&self) -> &str {
        self.resolved.as_deref().unwrap_or(&self.model)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Machine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_gb: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Settings {
    pub repeat: usize,
    pub num_ctx: u32,
    pub temperature: f64,
    pub max_image_side: u32,
    pub min_confidence: f64,
    pub tool_mode: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Recommendation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggest_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EvalResults {
    pub started: String,
    pub duration_s: i64,
    pub machine: Machine,
    pub ollama_version: String,
    pub settings: Settings,
    pub models: Vec<ModelReport>,
    pub recommendation: Recommendation,
}

// --- running ---------------------------------------------------------------------------

fn snap(name: &str) -> Snapshot {
    let s = scene(name).unwrap_or_else(|| panic!("unknown scene {name:?}"));
    Snapshot::with_image(WindowInfo::new(s.title, s.app), image::DynamicImage::ImageRgb8((s.render)()))
}

fn mark(b: Option<bool>) -> &'static str {
    match b {
        None => "–",
        Some(true) => "✓",
        Some(false) => "✗",
    }
}

pub struct EvalRunner {
    cfg: Config,
    repeat: usize,
    only: Option<String>,
    select: Option<String>,
    timeout: Duration,
    min_conf: f64,
}

impl EvalRunner {
    pub fn new(cfg: Config, opts: &EvalOptions) -> Self {
        let timeout = if opts.timeout_s > 0.0 { opts.timeout_s } else { 600.0 };
        Self {
            min_conf: cfg.suggestions.min_confidence,
            cfg,
            repeat: opts.repeat.max(1),
            only: opts.only.clone(),
            select: opts.filter.clone(),
            timeout: Duration::from_secs_f64(timeout),
        }
    }

    fn wanted(&self, name: &str) -> bool {
        self.select.as_deref().is_none_or(|s| s.is_empty() || name.contains(s))
    }

    fn runs(&self, half: &str) -> bool {
        self.only.as_deref().is_none_or(|o| o == half)
    }

    // --- one model ------------------------------------------------------------------
    pub async fn evaluate(&self, name: &str) -> ModelReport {
        let mut report = ModelReport { model: name.into(), status: "ok".into(), ..Default::default() };
        let mut mcfg = self.cfg.model.clone();
        mcfg.name = name.into();
        let llm = Arc::new(Ollama::new(&mcfg));
        let installed = match llm.installed_models().await {
            Ok(i) => i,
            Err(e) => {
                report.status = "error".into();
                report.error = Some(format!("Ollama unreachable: {e}"));
                return report;
            }
        };
        let Some(resolved) = find_installed(name, &installed) else {
            report.status = "missing".into();
            report.error = Some(format!("not installed — run: ollama pull {name}"));
            return report;
        };
        llm.set_model(&resolved);
        report.resolved = Some(resolved.clone());

        if let Err(e) = self.evaluate_loaded(&llm, &resolved, &mut report).await {
            report.status = "error".into();
            report.error = Some(e.to_string());
        }
        llm.unload().await;
        report.summary = Some(summarize(&report));
        report
    }

    async fn evaluate_loaded(&self, llm: &Arc<Ollama>, resolved: &str, report: &mut ModelReport) -> LlmResult<()> {
        unload_all(llm).await;
        println!("\n▶ {resolved}: loading…");
        let t0 = Instant::now();
        llm.chat(&[json!({"role": "user", "content": "Reply with OK."})], None, None, None).await?;
        report.load_s = Some(round_to(t0.elapsed().as_secs_f64(), 1));
        if let Some(ps) = llm.loaded().await {
            let size = ps["size"].as_f64().unwrap_or(0.0);
            let vram = ps["size_vram"].as_f64().unwrap_or(0.0);
            report.size_gb = Some(round_to(size / 1e9, 1));
            report.vram_pct = (size > 0.0).then(|| round_int(100.0 * vram / size));
            report.context = Some(ps.get("context_length").cloned().unwrap_or(Value::Null));
        }
        let none = || "None".to_string();
        println!(
            "  loaded in {}s, {} GB, {}% on GPU",
            report.load_s.map_or_else(none, pyf),
            report.size_gb.map_or_else(none, pyf),
            report.vram_pct.map_or_else(none, |v| v.to_string())
        );

        if self.runs("suggest") {
            for case in &SUGGEST_CASES {
                if self.wanted(case.scene) {
                    for _ in 0..self.repeat {
                        report.suggest.push(self.suggest_case(llm, case).await);
                    }
                }
            }
        }
        if self.runs("agent") {
            // Share what we learn about native tool support across this model's tasks.
            let modes = ToolModes::default();
            for task in &agent_tasks() {
                if self.wanted(task.name) {
                    for _ in 0..self.repeat {
                        report.agent.push(self.agent_task(llm, task, &modes).await);
                    }
                }
            }
            let used: BTreeSet<&str> = report.agent.iter().filter_map(|a| a.mode.as_deref()).collect();
            report.tool_mode = Some(used.into_iter().collect::<Vec<_>>().join("/"));
        }
        Ok(())
    }

    async fn suggest_case(&self, llm: &Arc<Ollama>, case: &SuggestCase) -> SuggestRecord {
        let snap = snap(case.scene);
        let sug = Suggester::new(llm.clone(), self.cfg.suggestions.clone(), self.cfg.model.max_image_side);
        let mut rec =
            SuggestRecord { case: case.scene.into(), expect: case.expect.map(String::from), ..Default::default() };
        let t0 = Instant::now();
        let (activity, raw) = match tokio::time::timeout(self.timeout, sug.propose(&snap)).await {
            Ok(Ok(found)) => {
                rec.json_ok = true;
                found
            }
            Ok(Err(e)) => {
                rec.error = Some(truncate(&e.to_string(), 300).to_string());
                (String::new(), vec![])
            }
            Err(_) => {
                rec.error = Some("timeout".into());
                (String::new(), vec![])
            }
        };
        rec.latency_s = round_to(t0.elapsed().as_secs_f64(), 1);
        rec.tok_s = if rec.json_ok { llm.stats().tok_s() } else { None };

        let act = activity.to_lowercase();
        rec.understood = case.activity_any.iter().any(|k| act.contains(k));
        let confident: Vec<&Value> =
            raw.iter().filter(|s| py_float(s.get("confidence")).is_some_and(|c| c >= self.min_conf)).collect();
        rec.behaved = match case.expect {
            Some("suggest") => Some(confident.iter().any(|s| {
                let text = |k: &str| s.get(k).map_or(String::new(), |v| py_str(Some(v)));
                let hay = format!("{} {}", text("title"), text("prompt")).to_lowercase();
                case.suggest_any.iter().any(|k| hay.contains(k))
            })),
            Some("quiet") => Some(confident.is_empty()),
            _ => None,
        };
        rec.activity = activity;
        rec.suggestions = raw.clone();
        let top = confident.first().map_or_else(|| "(quiet)".to_string(), |s| py_str(s.get("title")));
        println!(
            "  suggest {:<12} understand {}  behave {}  {:>6}s  {top}",
            case.scene,
            mark(Some(rec.understood)),
            mark(rec.behaved),
            pyf(rec.latency_s)
        );
        rec
    }

    async fn agent_task(&self, llm: &Arc<Ollama>, task: &AgentTask, modes: &ToolModes) -> AgentRecord {
        let mocks = Mocks::new();
        let agent = Agent::new(llm.clone(), mocks.registry(), self.cfg.model.clone(), modes.clone());
        let mut rec = AgentRecord { case: task.name.into(), prompt: task.prompt.into(), ..Default::default() };
        let before = llm.totals();
        let t0 = Instant::now();
        let snap = snap(task.scene);
        let on_status = |_: &str| {};
        let outcome = tokio::time::timeout(self.timeout, agent.run(task.prompt, &snap, &[], &on_status)).await;
        rec.latency_s = round_to(t0.elapsed().as_secs_f64(), 1);
        let result = match outcome {
            Ok(Ok(r)) => r,
            failed => {
                let msg = match failed {
                    Ok(Err(e)) => truncate(&e.to_string(), 300).to_string(),
                    _ => String::new(),
                };
                let error = if msg.is_empty() { "timeout".to_string() } else { msg };
                println!("  agent   {:<18} ERROR {}", task.name, truncate(&error, 80));
                rec.error = Some(error);
                rec.checks = Checks(task.checks.iter().map(|(k, _)| (k.to_string(), false)).collect());
                rec.passed = false;
                return rec;
            }
        };
        let after = llm.totals();
        rec.llm_calls = Some(after.calls.saturating_sub(before.calls));
        rec.llm_eval_count = Some(after.eval_count.saturating_sub(before.eval_count));
        rec.llm_thinking_chars = Some(after.thinking_chars.saturating_sub(before.thinking_chars));
        rec.mode = Some(result.mode.clone());
        rec.answer = Some(result.text.clone());
        rec.calls =
            Some(result.calls.iter().map(|c| CallRecord { name: c.name.clone(), args: c.args.clone() }).collect());
        rec.inserted = Some(mocks.inserted());
        rec.delegated = Some(mocks.delegated());
        rec.checks = Checks(task.checks.iter().map(|(k, f)| (k.to_string(), f(&result, &mocks))).collect());
        rec.passed = rec.checks.all();
        let steps = result.steps();
        println!(
            "  agent   {:<18} {}/{} checks  {:>6}s  {:>5} tok  [{}]",
            task.name,
            rec.checks.passed(),
            rec.checks.0.len(),
            pyf(rec.latency_s),
            rec.llm_eval_count.unwrap_or(0),
            if steps.is_empty() { "no tools".to_string() } else { steps.join(", ") }
        );
        rec
    }

    // --- all models -----------------------------------------------------------------
    pub async fn run(&self, models: &[String]) -> EvalResults {
        let started = chrono::Local::now();
        let t0 = Instant::now();
        let mut reports = vec![];
        for m in models {
            reports.push(self.evaluate(m).await);
        }
        let duration_s = round_int(t0.elapsed().as_secs_f64());
        let m = &self.cfg.model;
        let recommendation = recommend(&reports);
        EvalResults {
            started: started.format("%Y-%m-%dT%H:%M:%S").to_string(),
            duration_s,
            machine: machine_info(),
            ollama_version: Ollama::new(m).version().await,
            settings: Settings {
                repeat: self.repeat,
                num_ctx: m.num_ctx,
                temperature: m.temperature,
                max_image_side: m.max_image_side,
                min_confidence: self.min_conf,
                tool_mode: m.tool_mode.clone(),
            },
            models: reports,
            recommendation,
        }
    }
}

/// Free VRAM (including the candidate itself) so every model gets a cold, equal start.
async fn unload_all(llm: &Ollama) {
    let Ok(running) = llm.running().await else { return };
    for m in &running {
        if let Some(name) = m["name"].as_str() {
            llm.unload_model(name).await;
        }
    }
    for _ in 0..20 {
        // unloading is asynchronous
        match llm.running().await {
            Ok(r) if !r.is_empty() => tokio::time::sleep(Duration::from_millis(500)).await,
            _ => break,
        }
    }
}

fn ratio<T>(items: &[&T], key: impl Fn(&T) -> Option<bool>) -> (usize, usize) {
    let scored: Vec<bool> = items.iter().filter_map(|r| key(r)).collect();
    (scored.iter().filter(|b| **b).count(), scored.len())
}

pub fn summarize(r: &ModelReport) -> Summary {
    let sug: Vec<&SuggestRecord> = r.suggest.iter().collect();
    let by = |e: &str| -> Vec<&SuggestRecord> { r.suggest.iter().filter(|s| s.expect.as_deref() == Some(e)).collect() };
    let und = ratio(&sug, |s| Some(s.understood));
    let beh_s = ratio(&by("suggest"), |s| s.behaved);
    let beh_q = ratio(&by("quiet"), |s| s.behaved);
    let checks: Vec<bool> = r.agent.iter().flat_map(|a| a.checks.0.iter().map(|(_, v)| *v)).collect();
    let frac = |p: (usize, usize)| (p.1 > 0).then(|| p.0 as f64 / p.1 as f64);
    let passed_checks = checks.iter().filter(|b| **b).count();
    let parts = [
        (frac(und), 0.2),
        (frac(beh_s), 0.15),
        (frac(beh_q), 0.15),
        ((!checks.is_empty()).then(|| passed_checks as f64 / checks.len() as f64), 0.5),
    ];
    let have: Vec<(f64, f64)> = parts.iter().filter_map(|(v, w)| v.map(|v| (v, *w))).collect();
    let quality = (!have.is_empty())
        .then(|| 100.0 * have.iter().map(|(v, w)| w * v).sum::<f64>() / have.iter().map(|(_, w)| w).sum::<f64>());
    let suggest_parts: Vec<f64> = parts[..3].iter().filter_map(|(v, _)| *v).collect();
    let speeds: Vec<f64> = sug.iter().filter_map(|s| s.tok_s).filter(|t| *t != 0.0).collect();
    let ok_agents = || r.agent.iter().filter(|a| a.error.is_none());
    Summary {
        understanding: und,
        helpful: beh_s,
        quiet: beh_q,
        json_ok: ratio(&sug, |s| Some(s.json_ok)),
        agent_tasks: (r.agent.iter().filter(|a| a.passed).count(), r.agent.len()),
        agent_checks: (passed_checks, checks.len()),
        suggest_score: (!suggest_parts.is_empty())
            .then(|| round_to(suggest_parts.iter().sum::<f64>() / suggest_parts.len() as f64, 3)),
        quality: quality.map(|q| round_to(q, 1)),
        suggest_latency_s: median_f(&sug.iter().filter(|s| s.json_ok).map(|s| s.latency_s).collect::<Vec<_>>()),
        agent_latency_s: median_f(&ok_agents().map(|a| a.latency_s).collect::<Vec<_>>()),
        tok_s: (!speeds.is_empty()).then(|| round_to(speeds.iter().sum::<f64>() / speeds.len() as f64, 1)),
        tool_calls: median_i(
            &r.agent.iter().filter_map(|a| a.calls.as_ref().map(|c| c.len() as i64)).collect::<Vec<_>>(),
        ),
        agent_tokens: median_i(&r.agent.iter().filter_map(|a| a.llm_eval_count.map(|n| n as i64)).collect::<Vec<_>>()),
        thinking_chars: median_i(
            &r.agent.iter().filter_map(|a| a.llm_thinking_chars.map(|n| n as i64)).collect::<Vec<_>>(),
        ),
    }
}

pub fn recommend(reports: &[ModelReport]) -> Recommendation {
    let ok: Vec<(&ModelReport, &Summary)> = reports
        .iter()
        .filter(|r| r.status == "ok")
        .filter_map(|r| r.summary.as_ref().filter(|s| s.quality.is_some()).map(|s| (r, s)))
        .collect();
    let Some(&first) = ok.first() else { return Recommendation::default() };
    // First model with the highest quality, like Python's max().
    let (best, bs) = ok.iter().skip(1).fold(first, |acc, &cur| if cur.1.quality > acc.1.quality { cur } else { acc });
    let mut rec = Recommendation { name: Some(best.name().to_string()), suggest_name: None };
    let floor = bs.suggest_score.unwrap_or(0.0) - 0.1;
    let quickest = ok
        .iter()
        .filter(|(_, s)| s.suggest_score.unwrap_or(0.0) >= floor && s.suggest_latency_s.is_some())
        .min_by(|a, b| a.1.suggest_latency_s.unwrap().total_cmp(&b.1.suggest_latency_s.unwrap()));
    if let Some(&(q, qs)) = quickest {
        let b = bs.suggest_latency_s.unwrap_or(0.0);
        let ql = qs.suggest_latency_s.unwrap_or(0.0);
        if !std::ptr::eq(q, best) && b != 0.0 && ql != 0.0 && ql < 0.7 * b {
            rec.suggest_name = Some(q.name().to_string());
        }
    }
    rec
}

pub fn machine_info() -> Machine {
    let mut info = Machine::default();
    if which::which("nvidia-smi").is_ok() {
        let mut cmd = std::process::Command::new("nvidia-smi");
        cmd.args(["--query-gpu=name,memory.total", "--format=csv,noheader"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        if let Ok(out) = cmd.output() {
            info.gpu = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    // Linux only; elsewhere RAM is reported as unknown.
    if cfg!(target_os = "linux")
        && let Ok(text) = std::fs::read_to_string("/proc/meminfo")
    {
        let kb = text
            .lines()
            .find(|l| l.starts_with("MemTotal"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse::<f64>().ok());
        info.ram_gb = kb.map(|kb| round_int(kb / 1e6));
    }
    info
}

// --- reporting -----------------------------------------------------------------------

fn frac_str(p: (usize, usize)) -> String {
    if p.1 > 0 { format!("{}/{}", p.0, p.1) } else { "–".into() }
}

pub fn to_markdown(res: &EvalResults) -> String {
    let models = &res.models;
    let ok: Vec<&ModelReport> = models.iter().filter(|m| m.status == "ok").collect();
    let s = &res.settings;
    let mut lines: Vec<String> = vec![
        format!("# Fane model eval — {}", res.started),
        String::new(),
        format!(
            "Machine: {}, {} GB RAM · Ollama {} · num_ctx {} · temperature {} · image ≤{}px · repeat ×{} · took {} min",
            res.machine.gpu.as_deref().unwrap_or("unknown GPU"),
            res.machine.ram_gb.map_or_else(|| "?".to_string(), |r| r.to_string()),
            res.ollama_version,
            s.num_ctx,
            pyf(s.temperature),
            s.max_image_side,
            s.repeat,
            res.duration_s.div_euclid(60)
        ),
        String::new(),
    ];
    for m in models.iter().filter(|m| m.status != "ok") {
        lines.push(format!("- **{}**: {} — {}", m.model, m.status, m.error.as_deref().unwrap_or("")));
    }
    if ok.is_empty() {
        return lines.join("\n") + "\n";
    }

    let names: Vec<&str> = ok.iter().map(|m| m.name()).collect();
    lines.extend([
        String::new(),
        "## Summary".into(),
        String::new(),
        format!("| | {} |", names.join(" | ")),
        format!("|---|{}", "---|".repeat(ok.len())),
    ]);
    type Row = (&'static str, fn(&ModelReport, &Summary) -> String);
    let rows: [Row; 16] = [
        ("**Quality score** (0–100)", |_, s| dash_f(s.quality)),
        ("Screen understanding", |_, s| frac_str(s.understanding)),
        ("Helpful suggestion when expected", |_, s| frac_str(s.helpful)),
        ("Stays quiet when nothing to do", |_, s| frac_str(s.quiet)),
        ("Valid suggestion JSON", |_, s| frac_str(s.json_ok)),
        ("Agent tasks fully passed", |_, s| frac_str(s.agent_tasks)),
        ("Agent checks passed", |_, s| frac_str(s.agent_checks)),
        ("Tool calling", |m, _| m.tool_mode.clone().unwrap_or_else(|| "–".into())),
        ("Median suggestion latency (s)", |_, s| dash_f(s.suggest_latency_s)),
        ("Median agent task latency (s)", |_, s| dash_f(s.agent_latency_s)),
        ("Generation speed (tok/s)", |_, s| dash_f(s.tok_s)),
        ("Median tool calls per agent task", |_, s| dash(s.tool_calls)),
        ("Median tokens generated per agent task", |_, s| dash(s.agent_tokens)),
        ("Median hidden reasoning per task (chars)", |_, s| dash(s.thinking_chars)),
        ("Loaded size (GB) / on GPU", |m, _| format!("{} / {}%", dash_f(m.size_gb), dash(m.vram_pct))),
        ("Cold load (s)", |m, _| dash_f(m.load_s)),
    ];
    let empty = Summary::default();
    for (label, f) in rows {
        let cells: Vec<String> = ok.iter().map(|m| f(m, m.summary.as_ref().unwrap_or(&empty))).collect();
        lines.push(format!("| {label} | {} |", cells.join(" | ")));
    }

    if let Some(name) = &res.recommendation.name {
        lines.extend([
            String::new(),
            "## Recommended config".into(),
            String::new(),
            "```toml".into(),
            "[model]".into(),
            format!("name = \"{name}\""),
        ]);
        if let Some(sn) = &res.recommendation.suggest_name {
            lines.push(format!("suggest_name = \"{sn}\"   # faster for the proactive loop"));
        }
        lines.extend([
            "```".into(),
            String::new(),
            "Quality = 20% understanding + 15% helpful + 15% quiet + 50% agent checks. \
             A separate suggestion model is proposed only if it is ≥30% faster with a suggestion \
             score within 0.1 of the best model's."
                .into(),
        ]);
    }

    lines.extend([
        String::new(),
        "## Per case".into(),
        String::new(),
        format!("| case | {} |", names.join(" | ")),
        format!("|---|{}", "---|".repeat(ok.len())),
    ]);
    for case in &SUGGEST_CASES {
        let cells: Vec<String> = ok
            .iter()
            .map(|m| {
                let recs: Vec<&SuggestRecord> = m.suggest.iter().filter(|r| r.case == case.scene).collect();
                if recs.is_empty() {
                    return "–".into();
                }
                let cell: Vec<String> = recs
                    .iter()
                    .map(|r| {
                        let u = if r.understood { "U" } else { "u" };
                        let b = r.behaved.map_or("", |b| if b { "✓" } else { "✗" });
                        format!("{u}{b}")
                    })
                    .collect();
                let lat = median_f(&recs.iter().map(|r| r.latency_s).collect::<Vec<_>>());
                format!("{} · {}s", cell.join(" "), dash_f(lat))
            })
            .collect();
        lines.push(format!("| suggest: {} | {} |", case.scene, cells.join(" | ")));
    }
    for task in agent_tasks() {
        let cells: Vec<String> = ok
            .iter()
            .map(|m| {
                let recs: Vec<&AgentRecord> = m.agent.iter().filter(|r| r.case == task.name).collect();
                if recs.is_empty() {
                    return "–".into();
                }
                let cell: Vec<String> =
                    recs.iter().map(|r| format!("{}/{}", r.checks.passed(), r.checks.0.len())).collect();
                let lat = median_f(&recs.iter().map(|r| r.latency_s).collect::<Vec<_>>());
                format!("{} · {}s", cell.join(" "), dash_f(lat))
            })
            .collect();
        lines.push(format!("| agent: {} | {} |", task.name, cells.join(" | ")));
    }
    lines.extend([
        String::new(),
        "U/u = understood the screen or not; ✓/✗ = suggested (or stayed quiet) as expected.".into(),
    ]);

    lines.extend([String::new(), "## Failures".into(), String::new()]);
    let mut any_fail = false;
    for m in &ok {
        for r in &m.suggest {
            if !r.understood || r.behaved == Some(false) || !r.json_ok {
                any_fail = true;
                let sugs: Vec<String> = r
                    .suggestions
                    .iter()
                    .map(|s| format!("{} ({})", py_str(s.get("title")), py_str(s.get("confidence"))))
                    .collect();
                let sugs = if sugs.is_empty() { "none".to_string() } else { sugs.join("; ") };
                let err = r.error.as_ref().map_or(String::new(), |e| format!(" · error: {e}"));
                lines.push(format!(
                    "- **{}** suggest `{}`: activity “{}” · suggestions: {}{err}",
                    m.name(),
                    r.case,
                    truncate(&r.activity, 160),
                    truncate(&sugs, 200)
                ));
            }
        }
        for r in &m.agent {
            if !r.passed {
                any_fail = true;
                let failed: Vec<&str> = r.checks.0.iter().filter(|(_, v)| !v).map(|(k, _)| k.as_str()).collect();
                let calls = r.calls.as_deref().unwrap_or_default();
                let calls = if calls.is_empty() {
                    "none".into()
                } else {
                    calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
                };
                let answer = r.answer.as_deref().filter(|a| !a.is_empty()).or(r.error.as_deref()).unwrap_or("");
                lines.push(format!(
                    "- **{}** agent `{}`: failed {} · tools: {calls} · answer: “{}”",
                    m.name(),
                    r.case,
                    py_list_repr(&failed),
                    truncate(answer, 160)
                ));
            }
        }
    }
    if !any_fail {
        lines.push("None.".into());
    }
    lines.join("\n") + "\n"
}

/// Write results.json, report.md and the scene PNGs; returns the report's path.
pub fn save(res: &EvalResults, out_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    std::fs::write(out_dir.join("results.json"), serde_json::to_string_pretty(res)?)?;
    let md = out_dir.join("report.md");
    std::fs::write(&md, to_markdown(res))?;
    let dir = out_dir.join("scenes");
    std::fs::create_dir_all(&dir)?;
    for s in scenes() {
        (s.render)().save(dir.join(format!("{}.png", s.name)))?;
    }
    Ok(md)
}

/// The cases, as `fane eval --list` prints them.
pub fn list_cases() -> Vec<String> {
    let mut lines: Vec<String> = SUGGEST_CASES
        .iter()
        .map(|c| format!("suggest  {:<18} expect: {}", c.scene, c.expect.unwrap_or("not scored")))
        .collect();
    lines.extend(agent_tasks().iter().map(|t| format!("agent    {:<18} {}", t.name, t.prompt)));
    lines
}

/// Run the suite, print progress and the report, save results; returns the exit code.
pub async fn run(cfg: Config, opts: EvalOptions) -> anyhow::Result<i32> {
    let models: Vec<String> = if opts.models.is_empty() {
        RECOMMENDED_MODELS.iter().map(|s| s.to_string()).collect()
    } else {
        opts.models.clone()
    };
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| STATE_DIR.join("evals").join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()));
    let runner = EvalRunner::new(cfg, &opts);
    let res = runner.run(&models).await;
    let report = save(&res, &out)?;
    println!("\n{}", std::fs::read_to_string(&report)?);
    println!("Saved {} (full transcripts in results.json, screens in scenes/).", report.display());
    Ok(if res.models.iter().any(|m| m.status == "ok") { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sug(case: &str, expect: Option<&str>, understood: bool, behaved: Option<bool>, latency: f64) -> SuggestRecord {
        SuggestRecord {
            case: case.into(),
            expect: expect.map(String::from),
            json_ok: true,
            latency_s: latency,
            tok_s: Some(40.0),
            activity: format!("looking at {case}"),
            suggestions: vec![json!({"title": "Do a thing", "confidence": 0.9})],
            understood,
            behaved,
            ..Default::default()
        }
    }

    fn agent(case: &str, checks: &[bool], latency: f64, calls: usize, tokens: u64) -> AgentRecord {
        let checks = Checks(checks.iter().enumerate().map(|(i, v)| (format!("check {i}"), *v)).collect());
        AgentRecord {
            case: case.into(),
            prompt: "do it".into(),
            latency_s: latency,
            llm_calls: Some(2),
            llm_eval_count: Some(tokens),
            llm_thinking_chars: Some(0),
            mode: Some("native".into()),
            answer: Some("done".into()),
            calls: Some((0..calls).map(|_| CallRecord { name: "insert_text".into(), args: json!({}) }).collect()),
            inserted: Some(vec![]),
            delegated: Some(vec![]),
            passed: checks.all(),
            checks,
            ..Default::default()
        }
    }

    fn model(name: &str, suggest: Vec<SuggestRecord>, agent: Vec<AgentRecord>) -> ModelReport {
        let mut m = ModelReport {
            model: name.into(),
            status: "ok".into(),
            resolved: Some(name.into()),
            suggest,
            agent,
            load_s: Some(12.0),
            size_gb: Some(20.5),
            vram_pct: Some(100),
            tool_mode: Some("native".into()),
            ..Default::default()
        };
        m.summary = Some(summarize(&m));
        m
    }

    fn fixture() -> Vec<ModelReport> {
        let a = model(
            "big:31b",
            vec![
                sug("email", Some("suggest"), true, Some(true), 4.0),
                sug("idle", Some("quiet"), false, Some(false), 6.0),
                sug("article", None, true, None, 5.0),
            ],
            vec![
                agent("reply_insert", &[true, true, false], 10.0, 1, 100),
                agent("screen_read", &[true, true, true], 20.0, 2, 300),
            ],
        );
        let b = model(
            "small:4b",
            vec![
                sug("email", Some("suggest"), true, Some(true), 1.0),
                sug("idle", Some("quiet"), true, Some(false), 1.0),
                sug("article", None, false, None, 2.0),
            ],
            vec![agent("reply_insert", &[false, false, false], 3.0, 0, 50)],
        );
        let missing = ModelReport {
            model: "nope:1b".into(),
            status: "missing".into(),
            error: Some("not installed — run: ollama pull nope:1b".into()),
            ..Default::default()
        };
        vec![a, b, missing]
    }

    #[test]
    fn summarize_and_recommend() {
        let reports = fixture();
        let s = reports[0].summary.clone().unwrap();
        assert_eq!(s.understanding, (2, 3));
        assert_eq!((s.helpful, s.quiet, s.json_ok), ((1, 1), (0, 1), (3, 3)));
        assert_eq!((s.agent_tasks, s.agent_checks), ((1, 2), (5, 6)));
        // (2/3 + 1 + 0) / 3
        assert_eq!(s.suggest_score, Some(0.556));
        // 100 * (0.2·2/3 + 0.15·1 + 0.15·0 + 0.5·5/6) / 1.0
        assert_eq!(s.quality, Some(70.0));
        assert_eq!((s.suggest_latency_s, s.agent_latency_s, s.tok_s), (Some(5.0), Some(15.0), Some(40.0)));
        assert_eq!((s.tool_calls, s.agent_tokens), (Some(Num::Float(1.5)), Some(Num::Float(200.0))));
        assert_eq!(s.thinking_chars, Some(Num::Float(0.0)));

        let s = reports[1].summary.clone().unwrap();
        assert_eq!(s.suggest_score, Some(0.556));
        // 100 * (0.2·2/3 + 0.15) / 1.0 = 28.33
        assert_eq!(s.quality, Some(28.3));
        assert_eq!((s.suggest_latency_s, s.tool_calls), (Some(1.0), Some(Num::Int(0))));

        // big wins on quality; small is ≥30% faster with the same suggestion score.
        let rec = recommend(&reports);
        assert_eq!(rec.name.as_deref(), Some("big:31b"));
        assert_eq!(rec.suggest_name.as_deref(), Some("small:4b"));
        assert_eq!(recommend(&reports[..1]).suggest_name, None);
        assert_eq!(recommend(&reports[2..]), Recommendation::default());
        assert_eq!(serde_json::to_string(&Recommendation::default()).unwrap(), "{}");
    }

    #[test]
    fn markdown_report() {
        let models = fixture();
        let res = EvalResults {
            started: "2026-11-07T14:05:00".into(),
            duration_s: 754,
            machine: Machine { gpu: Some("NVIDIA RTX 5090, 32607 MiB".into()), ram_gb: Some(64) },
            ollama_version: "0.13.0".into(),
            settings: Settings {
                repeat: 1,
                num_ctx: 8192,
                temperature: 0.3,
                max_image_side: 1280,
                min_confidence: 0.6,
                tool_mode: "auto".into(),
            },
            recommendation: recommend(&models),
            models,
        };
        let md = to_markdown(&res);
        assert!(
            md.starts_with("# Fane model eval — 2026-11-07T14:05:00\n\nMachine: NVIDIA RTX 5090, 32607 MiB, 64 GB RAM")
        );
        assert!(md.contains("image ≤1280px · repeat ×1 · took 12 min"));
        assert!(md.contains("- **nope:1b**: missing — not installed — run: ollama pull nope:1b"));
        assert!(md.contains("## Summary\n\n| | big:31b | small:4b |\n|---|---|---|\n"));
        assert!(md.contains("| **Quality score** (0–100) | 70.0 | 28.3 |"));
        assert!(md.contains("| Screen understanding | 2/3 | 2/3 |"));
        assert!(md.contains("| Median tool calls per agent task | 1.5 | 0 |"));
        assert!(md.contains("| Loaded size (GB) / on GPU | 20.5 / 100% | 20.5 / 100% |"));
        assert!(md.contains("name = \"big:31b\"\nsuggest_name = \"small:4b\"   # faster for the proactive loop"));
        assert!(md.contains("| suggest: email | U✓ · 4.0s | U✓ · 1.0s |"));
        assert!(md.contains("| suggest: article | U · 5.0s | u · 2.0s |"));
        assert!(md.contains("| suggest: code | – | – |"));
        assert!(md.contains("| agent: reply_insert | 2/3 · 10.0s | 0/3 · 3.0s |"));
        assert!(md.contains("## Failures"));
        assert!(
            md.contains("- **big:31b** suggest `idle`: activity “looking at idle” · suggestions: Do a thing (0.9)")
        );
        assert!(md.contains(
            "- **small:4b** agent `reply_insert`: failed ['check 0', 'check 1', 'check 2'] · tools: none · answer: “done”"
        ));
        assert!(md.ends_with("”\n"));

        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["models"][0]["agent"][0]["checks"]["check 2"], false);
        assert_eq!(json["models"][0]["summary"]["understanding"], json!([2, 3]));
        assert!(json["models"][2].get("summary").is_none());
    }

    #[test]
    fn list_has_all_cases() {
        let lines = list_cases();
        assert_eq!(lines.len(), 14);
        assert_eq!(lines.iter().filter(|l| l.starts_with("suggest  ")).count(), 7);
        assert_eq!(lines.iter().filter(|l| l.starts_with("agent    ")).count(), 7);
        assert_eq!(lines[0], "suggest  email              expect: suggest");
        assert_eq!(lines[6], "suggest  article            expect: not scored");
        assert_eq!(lines[7], "agent    reply_insert       Reply to Dana saying Thursday at 3pm works for me.");
    }

    #[test]
    fn python_formatting() {
        assert_eq!((pyf(5.0), pyf(0.3), pyf(12.25)), ("5.0".into(), "0.3".into(), "12.25".into()));
        assert_eq!((round_to(2.25, 1), round_to(0.35, 1), round_to(2.675, 2)), (2.2, 0.3, 2.67));
        assert_eq!((round_int(2.5), round_int(3.5)), (2, 4));
        assert_eq!(py_list_repr(&["a", "doesn't"]), "['a', \"doesn't\"]");
        assert_eq!(median_i(&[3, 1, 2]), Some(Num::Int(2)));
        assert_eq!(median_f(&[1.0, 2.0]), Some(1.5));
        assert_eq!(py_float(Some(&json!("0.7"))), Some(0.7));
        assert_eq!(py_float(Some(&json!("high"))), None);
        assert_eq!(py_float(None), Some(0.0));
    }
}
