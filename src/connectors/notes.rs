//! Plain-text / Markdown notes folders (Obsidian vaults, ~/Notes, ...).
// STUB: replaced by the connectors port.

use serde_json::Value;

pub struct Notes;

impl Notes {
    /// Keeps only the configured dirs that exist (after `config::expand`).
    pub fn new(_dirs: &[String]) -> Self {
        Notes
    }

    /// Ranked hits: [{path, title, score, modified, snippet}], or a hint string if no folders.
    pub async fn search(&self, _query: &str, _limit: usize) -> anyhow::Result<Value> {
        Ok(Value::String("No notes folders found.".into()))
    }

    /// Full text (first 20k chars) of a note inside a configured folder.
    pub async fn read(&self, _path: &str) -> anyhow::Result<String> {
        anyhow::bail!("not implemented")
    }
}
