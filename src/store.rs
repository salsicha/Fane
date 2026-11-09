//! Fane's local database (SQLite with full-text search): the index of files and capabilities,
//! and the user's preferences. Lives in the state directory; nothing leaves the machine.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::Connection;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    has_text INTEGER NOT NULL
);
-- Contentless: the text isn't stored twice (snippets are read from the file when needed).
CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(name, folder, body, content = '', contentless_delete = 1, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS caps (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    source TEXT NOT NULL,
    description TEXT NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS caps_fts USING fts5(name, source, description, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS prefs (
    id INTEGER PRIMARY KEY,
    about TEXT NOT NULL,
    text TEXT NOT NULL,
    refs TEXT NOT NULL,
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS prefs_fts USING fts5(about, text, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

const FILES_SCHEMA: &str = "2";

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        // The app and `fane` commands may use the database at the same time.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        // The file index's layout changed (contentless full-text table): rebuild it. It's only
        // derived data; preferences are kept.
        conn.execute_batch("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        let version: Option<String> =
            conn.query_row("SELECT value FROM meta WHERE key = 'files_schema'", [], |r| r.get(0)).ok();
        if version.as_deref() != Some(FILES_SCHEMA) {
            conn.execute_batch(
                "DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS files_fts; DELETE FROM meta WHERE key = 'files_scanned';",
            )?;
            if version.is_some() {
                conn.execute_batch("VACUUM;")?; // give the old index's space back
            }
        }
        conn.execute_batch(SCHEMA).context("creating Fane's database")?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('files_schema', ?1) ON CONFLICT(key) DO UPDATE SET value = ?1",
            [FILES_SCHEMA],
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Run `f` with the connection (callers on the async runtime should use spawn_blocking for
    /// anything big).
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().unwrap();
        f(&mut conn).map_err(Into::into)
    }

    pub fn meta(&self, key: &str) -> Option<String> {
        self.with(|c| c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))).ok()
    }

    pub fn set_meta(&self, key: &str, value: &str) {
        let _ = self.with(|c| {
            c.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                [key, value],
            )
        });
    }
}

/// An FTS5 query from free text: every word must match (as a prefix), quoted so punctuation
/// can't be read as query syntax. `any`: match any word instead.
pub fn fts_query(text: &str, any: bool) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| format!("\"{}\"*", w.to_lowercase()))
        .collect();
    (!words.is_empty()).then(|| words.join(if any { " OR " } else { " " }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_with_fts5() {
        let s = Store::in_memory().unwrap();
        s.with(|c| {
            c.execute(
                "INSERT INTO files_fts(rowid, name, folder, body) VALUES (1, 'budget 2026', 'documents', 'podcasts')",
                [],
            )?;
            c.execute("DELETE FROM files_fts WHERE rowid = 7", [])
        })
        .unwrap();
        let n: i64 = s
            .with(|c| {
                c.query_row("SELECT count(*) FROM files_fts WHERE files_fts MATCH ?1", ["\"podcast\"*"], |r| r.get(0))
            })
            .unwrap();
        assert_eq!(n, 1);
        s.set_meta("k", "v");
        assert_eq!(s.meta("k").as_deref(), Some("v"));
    }

    #[test]
    fn query_building() {
        assert_eq!(fts_query("Q3 budget!", false).as_deref(), Some("\"q3\"* \"budget\"*"));
        assert_eq!(fts_query("a b", false), None);
        assert_eq!(fts_query("dana email", true).as_deref(), Some("\"dana\"* OR \"email\"*"));
        assert!(fts_query("\"); DROP TABLE files; --", false).unwrap().contains("\"drop\"*"));
    }
}
