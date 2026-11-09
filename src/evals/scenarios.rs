//! Eval cases: what a good assistant should do on each synthetic screen.
//!
//! Checks are deliberately simple keyword/trace tests so results are reproducible
//! and cheap; read the saved transcripts for anything subtler.

use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::AgentResult;
use crate::connectors::builtin::{Handlers, builtin_tools};
use crate::tools::{Args, Tool, ToolRegistry, handler, params};

// --- proactive suggestions ----------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct SuggestCase {
    pub scene: &'static str,
    /// Screen understanding: the `activity` sentence should mention one of these.
    pub activity_any: &'static [&'static str],
    /// "suggest": a confident suggestion matching `suggest_any` is expected.
    /// "quiet":   nothing confident should be suggested.
    /// None:      suggestion behaviour isn't scored (either is reasonable).
    pub expect: Option<&'static str>,
    pub suggest_any: &'static [&'static str],
}

pub const SUGGEST_CASES: [SuggestCase; 7] = [
    SuggestCase {
        scene: "email",
        activity_any: &["dana", "design review", "email", "reschedul"],
        expect: Some("suggest"),
        suggest_any: &["reply", "respond", "dana"],
    },
    SuggestCase {
        scene: "code",
        activity_any: &["keyerror", "test", "error", "pytest", "debug"],
        expect: Some("suggest"),
        suggest_any: &["keyerror", "user_id", "fix", "test", "error"],
    },
    SuggestCase {
        scene: "chat",
        activity_any: &["priya", "q3", "slack", "summary"],
        expect: Some("suggest"),
        suggest_any: &["q3", "summary", "summar", "priya"],
    },
    SuggestCase {
        scene: "spreadsheet",
        activity_any: &["spreadsheet", "budget", "sheet", "div"],
        expect: Some("suggest"),
        suggest_any: &["div", "error", "formula", "podcast", "zero"],
    },
    SuggestCase {
        scene: "idle",
        activity_any: &["desktop", "idle", "nothing", "no app", "wallpaper", "home screen"],
        expect: Some("quiet"),
        suggest_any: &[],
    },
    SuggestCase {
        scene: "photos",
        activity_any: &["photo", "picture", "image", "files", "browsing"],
        expect: Some("quiet"),
        suggest_any: &[],
    },
    SuggestCase {
        scene: "article",
        activity_any: &["aurora", "wikipedia", "reading", "article"],
        expect: None,
        suggest_any: &[],
    },
];

// --- agent tasks ----------------------------------------------------------------------

pub const Q3_NOTE: &str = "# Q3 plan\n- Launch the mobile beta in August\n- Hire two engineers (backend + iOS)\n\
                           - Marketing budget: $40k, mostly podcasts\n- Retire the legacy billing service";
pub const CLIPBOARD: &str = "hey, cant make it tmrw, can we do fri instead? thx";
pub const CALENDAR: &str = "10:00-10:15 Standup; 14:30-15:30 Design review with Dana Kim (room 4B)";

pub fn dana_email() -> Value {
    json!({"from": "Dana Kim <dana@example.com>", "subject": "Can we move the design review?",
           "date": "2026-11-05T10:02:00", "body": "Something came up this afternoon. Could we push \
           today's design review (2:30pm) to later this week? Tuesday or Thursday afternoon both work."})
}

pub fn newsletter() -> Value {
    json!({"from": "Weekly Digest <news@example.com>", "subject": "This week in tech",
           "date": "2026-11-04T08:00:00", "body": "Top stories..."})
}

static NOTES_QUERY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)q3|quarter|plan|roadmap").unwrap());
static PREAMBLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(here('s| is)|sure|certainly|of course|okay)").unwrap());
static FAILURE_TASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)user_id|keyerror|test_get_profile|handlers").unwrap());
static BUSY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bno\b|not free|busy|design review|conflict|booked").unwrap());

/// The harnesses the mock `delegate` tool offers (the default pairing first).
pub fn mock_agents() -> Vec<(String, String)> {
    [
        ("codegem", "CodeGem (SuperQode), a coding agent running on the local model"),
        ("claude", "Claude Code (the user's Claude subscription)"),
    ]
    .iter()
    .map(|(n, d)| (n.to_string(), d.to_string()))
    .collect()
}

/// One recorded call of the mock `delegate` tool.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Delegation {
    pub agent: String,
    pub task: String,
    pub cwd: String,
}

/// Fake connectors that record what the model did.
#[derive(Debug, Clone, Default)]
pub struct Mocks {
    pub inserted: Arc<Mutex<Vec<String>>>,
    pub delegated: Arc<Mutex<Vec<Delegation>>>,
    /// File changes the model asked to make (all declined).
    pub file_changes: Arc<Mutex<Vec<Value>>>,
}

/// A small, made-up home folder for the file tools: the same for every run.
pub fn files_fixture() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("fane-eval-files-{}", std::process::id())).join("alex");
        let files: [(&str, &str); 5] = [
            ("Documents/Budget 2026.md", "# Marketing budget 2026\n\n| Channel | Budget |\n|---|---|\n| Search | $15,000 |\n| Social | $9,500 |\n| Events | $4,000 |\n| Podcasts | $3,000 |\n"),
            ("Documents/Budget 2025.md", "# Marketing budget 2025\n\nSearch $12,000; Social $8,000; Events $5,000; Podcasts: none.\n"),
            ("Documents/Trip notes.txt", "Lisbon in March. Hotel near Alfama.\n"),
            ("Downloads/invoice-1042.txt", "Invoice 1042: $480 for design work.\n"),
            (".ssh/id_ed25519", "SECRET KEY - must never be read\n"),
        ];
        for (rel, text) in files {
            let path = home.join(rel);
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let _ = std::fs::write(path, text);
        }
        home
    })
    .clone()
}

/// Python-style `seq[:n]` (negative `n` counts from the end).
fn head<T: Clone>(v: &[T], n: i64) -> Vec<T> {
    let len = v.len() as i64;
    let end = if n < 0 { (len + n).max(0) } else { n.min(len) };
    v[..end as usize].to_vec()
}

impl Mocks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inserted(&self) -> Vec<String> {
        self.inserted.lock().unwrap().clone()
    }

    pub fn delegated(&self) -> Vec<Delegation> {
        self.delegated.lock().unwrap().clone()
    }

    pub fn file_changes(&self) -> Vec<Value> {
        self.file_changes.lock().unwrap().clone()
    }

    pub fn registry(&self) -> ToolRegistry {
        let ins = self.inserted.clone();
        let insert_text = handler(move |a: Args| {
            let ins = ins.clone();
            async move {
                ins.lock().unwrap().push(a.str("text"));
                Ok(json!("inserted (eval)"))
            }
        });
        let insert_image = handler(|_| async { Ok(json!("error: no such file")) });
        let read_clipboard = handler(|_| async { Ok(json!(CLIPBOARD)) });
        let list_windows = handler(|_| async {
            Ok(json!([{"app": "firefox", "title": "Inbox - Gmail"}, {"app": "slack", "title": "#planning"},
                      {"app": "code", "title": "handlers.py — api"}]))
        });
        let search_notes = handler(|a: Args| async move {
            if NOTES_QUERY.is_match(&a.str("query")) {
                // Only the first item: a good answer has to open the note.
                return Ok(json!([{"path": "/home/alex/Notes/q3-plan.md", "title": "q3-plan",
                                  "snippet": "# Q3 plan - Launch the mobile beta in August …"}]));
            }
            Ok(json!([]))
        });
        let read_note = handler(|a: Args| async move {
            Ok(json!(if a.str("path").to_lowercase().contains("q3") { Q3_NOTE } else { "error: no such note" }))
        });
        let recent_emails = handler(|a: Args| async move {
            let mut msgs = vec![dana_email(), newsletter()];
            let query = a.str("query");
            if !query.is_empty() {
                let q = query.to_lowercase();
                let hits: Vec<Value> = msgs
                    .iter()
                    .filter(|m| {
                        let hay = format!(
                            "{}{}{}",
                            m["from"].as_str().unwrap_or(""),
                            m["subject"].as_str().unwrap_or(""),
                            m["body"].as_str().unwrap_or("")
                        )
                        .to_lowercase();
                        q.split_whitespace().any(|w| hay.contains(w))
                    })
                    .cloned()
                    .collect();
                if !hits.is_empty() {
                    msgs = hits;
                }
            }
            let limit = match a.int("limit", 10) {
                0 => 10,
                n => n,
            };
            Ok(Value::Array(head(&msgs, limit)))
        });
        let del = self.delegated.clone();
        let delegate = handler(move |a: Args| {
            let del = del.clone();
            async move {
                let d = Delegation { agent: a.str("agent"), task: a.str("task"), cwd: a.str("cwd") };
                let who = if d.agent.is_empty() { "codegem".to_string() } else { d.agent.clone() };
                del.lock().unwrap().push(d);
                Ok(json!(format!("{who}: replaced request.json['user_id'] with session['uid']; 12 tests pass.")))
            }
        });
        let handlers = Handlers {
            insert_text,
            insert_image,
            read_clipboard,
            list_windows,
            search_notes,
            read_note,
            recent_emails,
            delegate,
        };
        let mut reg = ToolRegistry::new();
        for tool in builtin_tools(handlers, &mock_agents()) {
            reg.add(tool);
        }
        // The real file tools on a made-up home folder; any change is declined and recorded.
        let asked = self.file_changes.clone();
        let confirm: crate::connectors::files::Confirm = Arc::new(move |details| {
            asked.lock().unwrap().push(details);
            Box::pin(async { false })
        });
        let cfg = crate::config::FilesCfg { roots: vec![files_fixture().display().to_string()], ..Default::default() };
        for tool in Arc::new(crate::connectors::files::Files::new(&cfg, confirm)).tools() {
            reg.add(tool);
        }
        // Stands in for a tool coming from a calendar MCP server.
        reg.add(Tool::new(
            "calendar__todays_events",
            "[calendar] List today's calendar events.",
            params(&[], &[]),
            handler(|_| async { Ok(json!(CALENDAR)) }),
            "Asking calendar",
        ));
        reg
    }
}

pub type Check = Box<dyn Fn(&AgentResult, &Mocks) -> bool + Send + Sync>;

fn all_text(r: &AgentResult, m: &Mocks) -> String {
    format!("{} {}", r.text, m.inserted().join(" ")).to_lowercase()
}

fn called(name: &'static str) -> Check {
    Box::new(move |r: &AgentResult, _: &Mocks| r.steps().contains(&name))
}

fn answer_has(words: &'static [&'static str]) -> Check {
    Box::new(move |r: &AgentResult, _: &Mocks| {
        let t = r.text.to_lowercase();
        words.iter().any(|w| t.contains(w))
    })
}

fn inserted_has(words: &'static [&'static str]) -> Check {
    Box::new(move |_: &AgentResult, m: &Mocks| {
        m.inserted().iter().any(|t| {
            let t = t.to_lowercase();
            words.iter().any(|w| t.contains(w))
        })
    })
}

fn no_preamble() -> Check {
    Box::new(|_: &AgentResult, m: &Mocks| m.inserted().last().is_some_and(|t| !PREAMBLE.is_match(t)))
}

/// At least n of the facts are mentioned; each fact is a list of alternative phrasings.
fn facts(n: usize, groups: &'static [&'static [&'static str]]) -> Check {
    Box::new(move |r: &AgentResult, m: &Mocks| {
        let t = all_text(r, m);
        groups.iter().filter(|g| g.iter().any(|w| t.contains(w))).count() >= n
    })
}

fn check(f: impl Fn(&AgentResult, &Mocks) -> bool + Send + Sync + 'static) -> Check {
    Box::new(f)
}

pub struct AgentTask {
    pub name: &'static str,
    pub scene: &'static str,
    pub prompt: &'static str,
    /// Named checks, in report order.
    pub checks: Vec<(&'static str, Check)>,
}

pub fn agent_tasks() -> Vec<AgentTask> {
    vec![
        AgentTask {
            name: "reply_insert",
            scene: "email",
            prompt: "Reply to Dana saying Thursday at 3pm works for me.",
            checks: vec![
                ("calls insert_text", called("insert_text")),
                ("mentions Thursday", inserted_has(&["thursday"])),
                ("no preamble in inserted text", no_preamble()),
            ],
        },
        AgentTask {
            name: "email_lookup",
            scene: "idle",
            prompt: "What did Dana email me about?",
            checks: vec![
                ("calls recent_emails", called("recent_emails")),
                ("answer: design review / reschedule", answer_has(&["design review", "reschedul", "move", "push"])),
            ],
        },
        AgentTask {
            name: "notes_summary",
            scene: "chat",
            prompt: "Summarize my notes on the Q3 plan so I can send it to Priya.",
            checks: vec![
                ("calls search_notes", called("search_notes")),
                ("opens the note", called("read_note")),
                (
                    "≥3 of 4 facts from the note",
                    facts(
                        3,
                        &[
                            &["mobile beta", "august"],
                            &["two engineers", "2 engineers", "hire"],
                            &["40k", "40,000", "$40"],
                            &["billing"],
                        ],
                    ),
                ),
            ],
        },
        AgentTask {
            name: "screen_read",
            scene: "code",
            prompt: "What's the error on my screen and on which line?",
            checks: vec![
                ("names KeyError", answer_has(&["keyerror"])),
                ("names line 42", answer_has(&["42"])),
                (
                    "answers from the screen (≤1 tool, no delegation/insert)",
                    check(|r, m| r.calls.len() <= 1 && m.delegated().is_empty() && m.inserted().is_empty()),
                ),
            ],
        },
        AgentTask {
            name: "delegate_fix",
            scene: "code",
            prompt: "This needs a proper fix across the codebase. Get the failing test passing in ~/code/api.",
            checks: vec![
                ("calls delegate", called("delegate")),
                (
                    "task describes the failure",
                    check(|_, m| m.delegated().iter().any(|d| FAILURE_TASK.is_match(&d.task))),
                ),
            ],
        },
        AgentTask {
            name: "clipboard_rewrite",
            scene: "chat",
            prompt: "Rewrite what's on my clipboard to sound professional and put it where I'm typing.",
            checks: vec![
                ("calls read_clipboard", called("read_clipboard")),
                ("calls insert_text", called("insert_text")),
                ("keeps the meaning (Friday)", inserted_has(&["friday", "fri"])),
                (
                    "drops the slang",
                    check(|_, m| m.inserted().last().is_some_and(|t| !t.to_lowercase().contains("thx"))),
                ),
            ],
        },
        AgentTask {
            name: "files_lookup",
            scene: "idle",
            prompt: "How much did I budget for podcasts this year? It's in one of my documents.",
            checks: vec![
                (
                    "looks for the file",
                    check(|r, _| r.steps().iter().any(|s| ["find_files", "search_files", "list_files"].contains(s))),
                ),
                (
                    "answer: $3,000 (2026, not 2025)",
                    check(|r, _| ["3,000", "3000", "3k", "3 000"].iter().any(|w| r.text.to_lowercase().contains(w))),
                ),
                ("doesn't try to change files", check(|_, m| m.file_changes().is_empty())),
            ],
        },
        AgentTask {
            name: "calendar_mcp",
            scene: "email",
            prompt: "Am I free at 2:30 this afternoon?",
            checks: vec![
                ("calls the calendar tool", called("calendar__todays_events")),
                ("answer: busy (design review)", check(|r, _| BUSY.is_match(&r.text))),
                ("doesn't insert or delegate", check(|_, m| m.inserted().is_empty() && m.delegated().is_empty())),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolCall;

    #[test]
    fn mock_tools_share_real_specs() {
        let reg = Mocks::new().registry();
        for name in ["insert_text", "search_notes", "recent_emails", "delegate", "calendar__todays_events"] {
            assert!(reg.contains(name), "{name}");
        }
    }

    #[test]
    fn reply_checks_score_a_good_and_bad_trace() {
        let tasks = agent_tasks();
        let task = tasks.iter().find(|t| t.name == "reply_insert").unwrap();
        let (good, bad) = (Mocks::new(), Mocks::new());
        *good.inserted.lock().unwrap() = vec!["Hi Dana, Thursday at 3pm works for me. Thanks, Alex".into()];
        *bad.inserted.lock().unwrap() = vec!["Here is a reply: Tuesday works.".into()];
        let ok = AgentResult {
            text: "Inserted.".into(),
            inserted: true,
            calls: vec![ToolCall { name: "insert_text".into(), args: json!({}), output: "inserted".into() }],
            mode: "native".into(),
        };
        assert!(task.checks.iter().all(|(_, f)| f(&ok, &good)));
        assert!(!task.checks.iter().all(|(_, f)| f(&ok, &bad)));
    }

    #[tokio::test]
    async fn mock_insert_records_text() {
        let m = Mocks::new();
        let reg = m.registry();
        let out = reg.call("insert_text", Args::from_value(json!({"text": "hello"}))).await;
        assert!(out.starts_with("inserted"));
        assert_eq!(m.inserted(), vec!["hello"]);
    }

    #[tokio::test]
    async fn mocks_return_canned_data() {
        let m = Mocks::new();
        let reg = m.registry();
        let notes = reg.call("search_notes", Args::from_value(json!({"query": "Q3 roadmap"}))).await;
        assert!(notes.contains("q3-plan.md"));
        assert_eq!(reg.call("search_notes", Args::from_value(json!({"query": "cats"}))).await, "[]");
        assert_eq!(reg.call("read_note", Args::from_value(json!({"path": "/x/q3-plan.md"}))).await, Q3_NOTE);
        let mail = reg.call("recent_emails", Args::from_value(json!({"query": "dana", "limit": 5}))).await;
        assert!(mail.contains("design review") && !mail.contains("Weekly Digest"));
        let one = reg.call("recent_emails", Args::from_value(json!({"limit": 1}))).await;
        assert!(one.contains("Dana") && !one.contains("Weekly Digest"));
        let out = reg.call("delegate", Args::from_value(json!({"agent": "codex", "task": "fix KeyError"}))).await;
        assert!(out.starts_with("codex: "));
        assert_eq!(m.delegated()[0].task, "fix KeyError");
        assert_eq!(reg.call("calendar__todays_events", Args::default()).await, CALENDAR);
    }

    #[test]
    fn check_helpers() {
        let m = Mocks::new();
        let r = AgentResult { text: "No, you have the design review then.".into(), ..Default::default() };
        let tasks = agent_tasks();
        let cal = tasks.iter().find(|t| t.name == "calendar_mcp").unwrap();
        let passed: Vec<bool> = cal.checks.iter().map(|(_, f)| f(&r, &m)).collect();
        assert_eq!(passed, vec![false, true, true]);
        m.inserted.lock().unwrap().push("Mobile beta in August; hire two engineers; $40k budget".into());
        let notes = tasks.iter().find(|t| t.name == "notes_summary").unwrap();
        assert!(notes.checks[2].1(&AgentResult::default(), &m));
    }
}
