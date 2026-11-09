//! The user's standing preferences about how things should be done ("reply to Dana formally",
//! "save invoices to ~/Documents/Invoices"), so they don't have to repeat them. Only what the
//! user says goes in; a new preference on the same topic replaces the old one, and a preference
//! is removed when something it depends on (a folder, a tool, a coding agent) is gone.

use std::sync::Arc;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::config::expand;
use crate::store::{Store, fts_query};

/// At or below this many preferences, every one is shown to the model.
const SHOW_ALL: usize = 25;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pref {
    pub id: i64,
    /// Short topic, e.g. "email replies to Dana".
    pub about: String,
    pub text: String,
    /// Things it depends on: "path:<p>" and "tool:<name>".
    pub refs: Vec<String>,
}

impl Pref {
    pub fn line(&self) -> String {
        format!("[{}] ({}) {}", self.id, self.about, self.text)
    }
}

fn normalize(about: &str) -> String {
    about.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Paths and tool/agent names a preference mentions.
pub fn extract_refs(text: &str, known_tools: &[String]) -> Vec<String> {
    let mut refs = vec![];
    // A path starts a word: ~/..., /..., or C:\...
    let path_re = regex::Regex::new(r#"(?:^|[\s(\["'])((?:~|/|[A-Za-z]:\\)[^\s"'`,;)]*)"#).unwrap();
    for m in path_re.captures_iter(text).filter_map(|c| c.get(1)) {
        let p = m.as_str().trim_end_matches(['.', ':']);
        if p.len() > 2 && (p.starts_with('~') || p.matches(['/', '\\']).count() >= 2 || p.contains(":\\")) {
            refs.push(format!("path:{p}"));
        }
    }
    let lower = text.to_lowercase();
    for tool in known_tools {
        let t = tool.to_lowercase();
        let found = lower.match_indices(&t).any(|(i, _)| {
            let before = lower[..i].chars().last().is_none_or(|c| !c.is_alphanumeric() && c != '_');
            let after = lower[i + t.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_');
            before && after
        });
        if found {
            refs.push(format!("tool:{tool}"));
        }
    }
    refs.sort();
    refs.dedup();
    refs
}

pub struct Prefs {
    store: Arc<Store>,
}

impl Prefs {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    fn row(r: &rusqlite::Row) -> rusqlite::Result<Pref> {
        let refs: String = r.get(3)?;
        Ok(Pref {
            id: r.get(0)?,
            about: r.get(1)?,
            text: r.get(2)?,
            refs: serde_json::from_str(&refs).unwrap_or_default(),
        })
    }

    pub fn list(&self) -> Vec<Pref> {
        self.store
            .with(|c| {
                c.prepare("SELECT id, about, text, refs FROM prefs ORDER BY id")?.query_map([], Self::row)?.collect()
            })
            .unwrap_or_default()
    }

    pub fn get(&self, id: i64) -> Option<Pref> {
        self.store
            .with(|c| c.query_row("SELECT id, about, text, refs FROM prefs WHERE id = ?1", [id], Self::row).optional())
            .ok()
            .flatten()
    }

    /// Save a preference. It replaces any with the same topic and the ids in `replaces`
    /// (which the model passes when the user changes an earlier instruction). Returns the new
    /// preference and the ones it replaced.
    pub fn save(
        &self,
        about: &str,
        text: &str,
        replaces: &[i64],
        known_tools: &[String],
    ) -> anyhow::Result<(Pref, Vec<Pref>)> {
        let about = about.trim();
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "the preference is empty");
        let about = if about.is_empty() { text.chars().take(40).collect::<String>() } else { about.to_string() };
        let key = normalize(&about);
        let old: Vec<Pref> =
            self.list().into_iter().filter(|p| replaces.contains(&p.id) || normalize(&p.about) == key).collect();
        let refs = extract_refs(text, known_tools);
        let now = chrono::Utc::now().timestamp();
        let id = self.store.with(|c| {
            let tx = c.transaction()?;
            for p in &old {
                tx.execute("DELETE FROM prefs WHERE id = ?1", [p.id])?;
                tx.execute("DELETE FROM prefs_fts WHERE rowid = ?1", [p.id])?;
            }
            tx.execute(
                "INSERT INTO prefs(about, text, refs, created, updated) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![about, text, serde_json::to_string(&refs).unwrap_or_default(), now],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute("INSERT INTO prefs_fts(rowid, about, text) VALUES (?1, ?2, ?3)", params![id, about, text])?;
            tx.commit()?;
            Ok(id)
        })?;
        Ok((Pref { id, about, text: text.to_string(), refs }, old))
    }

    pub fn forget(&self, id: i64) -> Option<Pref> {
        let p = self.get(id)?;
        let _ = self.store.with(|c| {
            c.execute("DELETE FROM prefs WHERE id = ?1", [id])?;
            c.execute("DELETE FROM prefs_fts WHERE rowid = ?1", [id])
        });
        Some(p)
    }

    /// The preferences worth showing for `task`: all of them when there are few, otherwise the
    /// best matches.
    pub fn relevant(&self, task: &str, limit: usize) -> Vec<Pref> {
        let all = self.list();
        if all.len() <= SHOW_ALL {
            return all;
        }
        let Some(q) = fts_query(task, true) else { return vec![] };
        self.store
            .with(|c| {
                c.prepare(
                    "SELECT p.id, p.about, p.text, p.refs FROM prefs_fts JOIN prefs p ON p.id = prefs_fts.rowid
                     WHERE prefs_fts MATCH ?1 ORDER BY bm25(prefs_fts, 2.0, 1.0) LIMIT ?2",
                )?
                .query_map(params![q, limit as i64], Self::row)?
                .collect()
            })
            .unwrap_or_default()
    }

    /// Remove preferences that depend on something that's gone: a path that no longer exists, or
    /// a tool/agent that is no longer configured (`still_configured` says whether a tool name
    /// still exists or belongs to an MCP server that's still configured). Returns what was removed
    /// and why.
    pub fn invalidate(&self, still_configured: &dyn Fn(&str) -> bool) -> Vec<(Pref, String)> {
        let mut removed = vec![];
        for p in self.list() {
            let reason = p.refs.iter().find_map(|r| {
                if let Some(path) = r.strip_prefix("path:") {
                    (!expand(path).exists()).then(|| format!("{path} no longer exists"))
                } else if let Some(tool) = r.strip_prefix("tool:") {
                    (!still_configured(tool)).then(|| format!("{tool} is no longer available"))
                } else {
                    None
                }
            });
            if let Some(reason) = reason {
                self.forget(p.id);
                removed.push((p, reason));
            }
        }
        removed
    }
}

/// The preferences section of a prompt (empty when there are none).
pub fn prompt_section(prefs: &[Pref]) -> String {
    if prefs.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = prefs.iter().map(|p| format!("- {}", p.line())).collect();
    format!(
        "\nThe user's standing preferences (follow them unless the user now says otherwise):\n{}\n",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> Prefs {
        Prefs::new(Arc::new(Store::in_memory().unwrap()))
    }

    #[test]
    fn same_topic_replaces_and_ids_replace() {
        let p = prefs();
        let (a, old) = p.save("email replies to Dana", "Keep replies to Dana formal.", &[], &[]).unwrap();
        assert!(old.is_empty());
        let (b, old) = p.save("Email replies to dana!", "Replies to Dana can be casual now.", &[], &[]).unwrap();
        assert_eq!(old, vec![a.clone()], "same topic (normalized) replaces");
        let (_, old) = p.save("Dana tone", "Casual, and sign as Alex.", &[b.id], &[]).unwrap();
        assert_eq!(old[0].id, b.id, "explicit replaces");
        assert_eq!(p.list().len(), 1);
        assert!(p.forget(999).is_none());
    }

    #[test]
    fn refs_and_invalidation() {
        let dir = std::env::temp_dir().join(format!("fane-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tools = vec!["codegem".to_string(), "calendar__list_events".to_string()];
        let refs =
            extract_refs(&format!("Save invoices to {}/Invoices. Use codegem for Python.", dir.display()), &tools);
        assert!(refs.iter().any(|r| r.starts_with("path:") && r.ends_with("/Invoices")), "{refs:?}");
        assert!(refs.contains(&"tool:codegem".to_string()));
        assert!(!extract_refs("decodegem", &tools).contains(&"tool:codegem".to_string()), "whole words only");
        assert!(extract_refs("replies and/or notes", &tools).is_empty(), "not a path");
        assert_eq!(extract_refs("keep it in ~/Notes.", &tools), vec!["path:~/Notes"]);

        let p = prefs();
        std::fs::create_dir_all(dir.join("Invoices")).unwrap();
        p.save("invoices", &format!("Save invoices to {}/Invoices", dir.display()), &[], &tools).unwrap();
        p.save("python", "Use codegem for Python work", &[], &tools).unwrap();
        p.save("tone", "Be brief.", &[], &tools).unwrap();
        assert!(p.invalidate(&|_| true).is_empty(), "nothing changed");
        std::fs::remove_dir_all(dir.join("Invoices")).unwrap();
        let removed = p.invalidate(&|t| t != "codegem");
        let reasons: Vec<&str> = removed.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(removed.len(), 2, "{reasons:?}");
        assert!(reasons.iter().any(|r| r.ends_with("no longer exists")));
        assert!(reasons.contains(&"codegem is no longer available"));
        assert_eq!(p.list().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prompt_section_lists_ids() {
        let p = prefs();
        p.save("tone", "Be brief.", &[], &[]).unwrap();
        let s = prompt_section(&p.relevant("anything", 10));
        assert!(s.contains("- [1] (tone) Be brief."));
        assert_eq!(prompt_section(&[]), "");
    }
}
