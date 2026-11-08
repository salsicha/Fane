//! Proactive loop: watch the screen, and occasionally prompt itself with a suggestion.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::SuggestionsCfg;
use crate::llm::{LlmResult, Ollama, encode_image};
use crate::snapshot::{Snapshot, change_ratio};

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "activity": {"type": "string"},
            "suggestions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "prompt": {"type": "string"},
                        "confidence": {"type": "number"},
                    },
                    "required": ["title", "prompt", "confidence"],
                },
            },
        },
        "required": ["activity", "suggestions"],
    })
}

fn prompt(window: &str, history: &str, recent: &str) -> String {
    format!(
        "You are the proactive half of Fane, a desktop assistant. Look at the user's screen.

1. In `activity`, describe in one short sentence what the user is doing right now.
2. In `suggestions`, propose AT MOST ONE thing the assistant could do immediately that would
   clearly help with that exact activity. The assistant can: read the user's notes and email,
   draft or rewrite text and insert it at the cursor, summarize what's on screen, look things
   up in open apps, and hand coding work to a coding agent.
   - `title`: 2-6 words shown on a small card, e.g. \"Draft a reply to Dana\".
   - `prompt`: a self-contained instruction the assistant will execute, written as the
     user would type it, mentioning concrete names/details visible on screen.
   - `confidence`: 0-1, how sure you are the user would want this right now.

Staying quiet is better than noise: return an empty list unless the help is specific and
valuable. Never suggest generic things (\"take a break\", \"organize files\", \"learn more\").
Focused window: {window}
Recent activity: {history}
Do not repeat these recent suggestions: {recent}
"
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub id: u64,
    pub title: String,
    pub prompt: String,
    pub confidence: f64,
    pub window: String,
    pub created_at: f64,
}

/// `confidence` as a number, tolerating strings and junk (-> 0).
pub fn confidence(s: &Value) -> f64 {
    match &s["confidence"] {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(t) => t.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub struct Suggester {
    llm: Arc<Ollama>,
    cfg: SuggestionsCfg,
    max_image_side: u32,
    pub history: Vec<String>,
    pub recent_titles: Vec<String>,
    last_snap: Option<Snapshot>,
    quiet_until: Option<Instant>,
    dismissals: u32,
    next_id: u64,
}

impl Suggester {
    pub fn new(llm: Arc<Ollama>, cfg: SuggestionsCfg, max_image_side: u32) -> Self {
        Self {
            llm,
            cfg,
            max_image_side,
            history: vec![],
            recent_titles: vec![],
            last_snap: None,
            quiet_until: None,
            dismissals: 0,
            next_id: 1,
        }
    }

    // --- feedback from the UI -------------------------------------------------
    pub fn dismissed(&mut self) {
        self.dismissals += 1;
        let factor = 2u64.saturating_pow(self.dismissals - 1);
        let delay = (self.cfg.cooldown_seconds.saturating_mul(factor)).min(self.cfg.max_cooldown_seconds);
        self.quiet_until = Some(Instant::now() + Duration::from_secs(delay));
        tracing::info!("suggestion dismissed; quiet for {delay}s");
    }

    pub fn ignored(&mut self) {
        let until = Instant::now() + Duration::from_secs(self.cfg.cooldown_seconds);
        self.quiet_until = Some(self.quiet_until.map_or(until, |q| q.max(until)));
    }

    pub fn accepted(&mut self) {
        self.dismissals = 0;
    }

    // --- main step ------------------------------------------------------------
    pub fn should_look(&self, snap: &Snapshot, force: bool) -> bool {
        if force {
            return true;
        }
        if self.quiet_until.is_some_and(|q| Instant::now() < q) {
            return false;
        }
        let Some(last) = &self.last_snap else { return true };
        if last.window.label() != snap.window.label() {
            return true;
        }
        change_ratio(last.image.as_deref(), snap.image.as_deref()) >= self.cfg.min_change
    }

    fn is_repeat(&self, title: &str) -> bool {
        let t = title.to_lowercase();
        let start = self.recent_titles.len().saturating_sub(20);
        self.recent_titles[start..].iter().any(|r| similarity(&t, &r.to_lowercase()) > 0.75)
    }

    /// One model call: returns (activity, raw suggestions), with no filtering.
    pub async fn propose(&self, snap: &Snapshot) -> LlmResult<(String, Vec<Value>)> {
        let tail = |v: &[String], n: usize| v[v.len().saturating_sub(n)..].to_vec();
        let history = tail(&self.history, 4).join(" | ");
        let recent = tail(&self.recent_titles, 8).join("; ");
        let text = prompt(
            &snap.window.label(),
            if history.is_empty() { "none" } else { &history },
            if recent.is_empty() { "none" } else { &recent },
        );
        let mut msg = json!({"role": "user", "content": text});
        if let Some(img) = &snap.image {
            msg["images"] = json!([encode_image(img, self.max_image_side)]);
        }
        let data = self.llm.chat_json(&[msg], &schema()).await?;
        if !data.is_object() {
            return Ok((String::new(), vec![]));
        }
        let raw = data["suggestions"].as_array().cloned().unwrap_or_default();
        Ok((text_of(&data["activity"]), raw.into_iter().filter(Value::is_object).collect()))
    }

    pub async fn step(&mut self, snap: &Snapshot, force: bool) -> LlmResult<Option<Suggestion>> {
        if snap.image.is_none() || !self.should_look(snap, force) {
            return Ok(None);
        }
        self.last_snap = Some(snap.clone());
        let (activity, raw) = self.propose(snap).await?;
        if !activity.is_empty() {
            self.history.push(activity.clone());
            let excess = self.history.len().saturating_sub(12);
            self.history.drain(..excess);
        }
        tracing::info!("activity: {activity}");
        let min_conf = if force { 0.0 } else { self.cfg.min_confidence };
        for s in raw {
            let (title, text, conf) = (text_of(&s["title"]), text_of(&s["prompt"]), confidence(&s));
            if title.is_empty() || text.is_empty() || conf < min_conf || (self.is_repeat(&title) && !force) {
                tracing::info!("skipped suggestion {title:?} ({conf:.2})");
                continue;
            }
            self.recent_titles.push(title.clone());
            let sug = Suggestion {
                id: self.next_id,
                title,
                prompt: text,
                confidence: conf,
                window: snap.window.label(),
                created_at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            };
            self.next_id += 1;
            return Ok(Some(sug));
        }
        Ok(None)
    }
}

/// Python's `difflib.SequenceMatcher(None, a, b).ratio()`: 2·matches / total length.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    2.0 * matching(&a, &b) as f64 / (a.len() + b.len()) as f64
}

/// Total size of the matching blocks found by recursively taking the longest common run.
#[allow(clippy::needless_range_loop)]
fn matching(a: &[char], b: &[char]) -> usize {
    let (mut best, mut best_i, mut best_j) = (0, 0, 0);
    let mut prev = vec![0usize; b.len() + 1];
    for i in 0..a.len() {
        let mut cur = vec![0usize; b.len() + 1];
        for j in 0..b.len() {
            if a[i] == b[j] {
                cur[j + 1] = prev[j] + 1;
                if cur[j + 1] > best {
                    best = cur[j + 1];
                    best_i = i + 1 - best;
                    best_j = j + 1 - best;
                }
            }
        }
        prev = cur;
    }
    if best == 0 {
        return 0;
    }
    best + matching(&a[..best_i], &b[..best_j]) + matching(&a[best_i + best..], &b[best_j + best..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_matches_difflib() {
        assert_eq!(similarity("abcd", "abcd"), 1.0);
        assert_eq!(similarity("abcd", "wxyz"), 0.0);
        // difflib.SequenceMatcher(None, "draft a reply to dana", "draft reply to dana").ratio()
        assert!((similarity("draft a reply to dana", "draft reply to dana") - 0.95).abs() < 1e-9);
        assert!(similarity("fix the keyerror", "summarize q3 notes") < 0.75);
    }

    #[test]
    fn dismissals_back_off() {
        let llm = Arc::new(Ollama::new(&Default::default()));
        let mut s = Suggester::new(llm, SuggestionsCfg::default(), 1280);
        let snap = Snapshot::with_image(Default::default(), image::DynamicImage::new_rgb8(4, 4));
        assert!(s.should_look(&snap, false));
        s.dismissed();
        assert!(!s.should_look(&snap, false));
        assert!(s.should_look(&snap, true));
        s.accepted();
        assert_eq!(s.dismissals, 0);
    }

    #[test]
    fn confidence_is_lenient() {
        assert_eq!(confidence(&json!({"confidence": 0.8})), 0.8);
        assert_eq!(confidence(&json!({"confidence": "0.5"})), 0.5);
        assert_eq!(confidence(&json!({"confidence": "high"})), 0.0);
        assert_eq!(confidence(&json!({})), 0.0);
    }
}
