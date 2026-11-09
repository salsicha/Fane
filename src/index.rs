//! What Fane knows is where: the files in the allowed folders (names for everything, text for
//! documents) and the capabilities it can use (built-in tools, MCP tools, Fane's email, coding
//! agents). Entries are invalidated when they stop being accurate: a periodic scan re-indexes
//! files whose size or date changed and drops deleted ones, a watcher catches changes as they
//! happen where the OS allows, file tools report their own changes, every search result is
//! re-checked before it's returned, and capabilities are replaced whenever the set of tools
//! changes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{FilesCfg, expand};
use crate::connectors::files::wildcard;
use crate::store::{Store, fts_query};

/// Document-like files whose text is indexed (code is left to the coding agents).
const TEXT_EXT: [&str; 13] =
    ["md", "markdown", "txt", "org", "rst", "csv", "tsv", "tex", "html", "htm", "eml", "adoc", "rtf"];
const MAX_TEXT_FILE: u64 = 512 * 1024;
const MAX_TEXT_INDEXED: usize = 64 * 1024;
/// Folders not worth indexing (big and generated).
const SKIP_DIRS: [&str; 8] =
    ["node_modules", "target", "__pycache__", "venv", "dist", "build", "site-packages", "vendor"];

/// Tool installs and app internals directly inside a root (e.g. ~/anaconda3): huge, and not
/// the user's own files.
const SKIP_TOP: [&str; 8] =
    ["anaconda3", "miniconda3", "miniforge3", "snap", "go", "Library", "AppData", "Applications"];

/// The folders and rules the index follows (the same ones as the file tools).
#[derive(Debug, Clone)]
pub struct Scope {
    pub roots: Vec<PathBuf>,
    pub blocked: Vec<String>,
}

impl Scope {
    pub fn from_cfg(cfg: &FilesCfg) -> Self {
        let roots = cfg.roots.iter().map(|r| expand(r)).filter_map(|p| p.canonicalize().ok()).collect();
        Self { roots, blocked: cfg.blocked.clone() }
    }

    fn skip_name(&self, name: &str, is_dir: bool) -> bool {
        name.starts_with('.')
            || (is_dir && SKIP_DIRS.contains(&name))
            || (!is_dir && self.blocked.iter().any(|b| wildcard(b, name)))
    }

    /// Whether `path` (canonical) is inside a root and not hidden or blocked.
    pub fn contains(&self, path: &Path) -> bool {
        let Some(root) = self.roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.as_os_str().len()) else {
            return false;
        };
        let rel = path.strip_prefix(root).unwrap_or(path);
        let parts: Vec<_> = rel.components().collect();
        parts.iter().enumerate().all(|(i, c)| {
            let name = c.as_os_str().to_string_lossy();
            let is_dir = i + 1 < parts.len();
            !(self.skip_name(&name, is_dir) || (i == 0 && is_dir && SKIP_TOP.contains(&name.as_ref())))
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileHit {
    pub path: String,
    pub modified: String,
    pub size: u64,
    /// A few words around the match, for documents.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub snippet: String,
}

/// Something Fane can use to get a task done.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capability {
    /// "tool" (callable now) or "agent" (a coding agent reached through `delegate`).
    pub kind: String,
    pub name: String,
    /// Where it comes from: "built-in", "MCP server <name>", "Fane's email", "coding agent".
    pub source: String,
    pub description: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ScanStats {
    pub files: usize,
    pub added_or_changed: usize,
    pub removed: usize,
}

pub struct Index {
    store: Arc<Store>,
    scope: std::sync::RwLock<Scope>,
    scanning: AtomicBool,
}

fn mtime(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn when(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn read_text(path: &Path, size: u64) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if !TEXT_EXT.contains(&ext.as_str()) || size > MAX_TEXT_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(4096).any(|b| *b == 0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_TEXT_INDEXED)]).into_owned();
    Some(if ext == "html" || ext == "htm" {
        regex::Regex::new(r"<[^>]+>").unwrap().replace_all(&text, " ").into_owned()
    } else {
        text
    })
}

/// A one-line excerpt around the first query word found in a document.
fn snippet_of(path: &Path, size: u64, query: &str) -> String {
    let Some(text) = read_text(path, size) else { return String::new() };
    let lower = text.to_lowercase();
    let at = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .filter_map(|w| lower.find(&w.to_lowercase()))
        .min();
    let Some(at) = at else { return String::new() };
    let start = (0..=at.saturating_sub(60)).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    let end = (at + 120).min(text.len());
    let end = (end..=text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    let excerpt: String = text[start..end].split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}{excerpt}{}", if start > 0 { "…" } else { "" }, if end < text.len() { "…" } else { "" })
}

/// Folder words for matching ("Documents Invoices 2026"), from the path below its root.
fn folder_words(path: &Path) -> String {
    path.parent()
        .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

fn upsert(tx: &rusqlite::Transaction, path: &Path, size: u64, mtime: i64) -> rusqlite::Result<()> {
    let p = path.display().to_string();
    let text = read_text(path, size);
    let existing: Option<i64> = tx.query_row("SELECT id FROM files WHERE path = ?1", [&p], |r| r.get(0)).optional()?;
    let id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE files SET size = ?1, mtime = ?2, has_text = ?3 WHERE id = ?4",
                params![size as i64, mtime, text.is_some(), id],
            )?;
            tx.execute("DELETE FROM files_fts WHERE rowid = ?1", [id])?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO files(path, size, mtime, has_text) VALUES (?1, ?2, ?3, ?4)",
                params![p, size as i64, mtime, text.is_some()],
            )?;
            tx.last_insert_rowid()
        }
    };
    let name = path.file_name().map(|n| n.to_string_lossy().replace(['_', '-', '.'], " ")).unwrap_or_default();
    tx.execute(
        "INSERT INTO files_fts(rowid, name, folder, body) VALUES (?1, ?2, ?3, ?4)",
        params![id, name, folder_words(path), text.unwrap_or_default()],
    )?;
    Ok(())
}

fn remove(tx: &rusqlite::Transaction, path: &str) -> rusqlite::Result<bool> {
    let id: Option<i64> = tx.query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0)).optional()?;
    if let Some(id) = id {
        tx.execute("DELETE FROM files_fts WHERE rowid = ?1", [id])?;
        tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
    }
    Ok(id.is_some())
}

impl Index {
    pub fn new(store: Arc<Store>, scope: Scope) -> Self {
        Self { store, scope: std::sync::RwLock::new(scope), scanning: AtomicBool::new(false) }
    }

    pub fn scope(&self) -> Scope {
        self.scope.read().unwrap().clone()
    }

    /// New folders (Settings changed): files outside them are dropped on the next scan.
    pub fn set_scope(&self, scope: Scope) {
        *self.scope.write().unwrap() = scope;
    }

    pub fn ready(&self) -> bool {
        self.store.meta("files_scanned").is_some()
    }

    pub fn file_count(&self) -> usize {
        self.store.with(|c| c.query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))).unwrap_or(0) as usize
    }

    // --- files -------------------------------------------------------------------------------
    /// Bring the file index up to date: index new and changed files, drop deleted ones and any
    /// outside the allowed folders. Blocking; safe to call while searches run.
    pub fn scan(&self) -> anyhow::Result<ScanStats> {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Ok(ScanStats::default());
        }
        let result = self.scan_inner();
        self.scanning.store(false, Ordering::SeqCst);
        result
    }

    fn scan_inner(&self) -> anyhow::Result<ScanStats> {
        let scope = self.scope();
        let known: HashMap<String, (i64, i64)> = self.store.with(|c| {
            let mut st = c.prepare("SELECT path, size, mtime FROM files")?;
            let rows =
                st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))))?;
            rows.collect()
        })?;
        let mut stats = ScanStats::default();
        let mut seen: HashSet<String> = HashSet::with_capacity(known.len());
        let mut batch: Vec<(PathBuf, u64, i64)> = vec![];
        let flush = |batch: &mut Vec<(PathBuf, u64, i64)>| -> anyhow::Result<()> {
            self.store.with(|c| {
                let tx = c.transaction()?;
                for (p, size, mtime) in batch.iter() {
                    upsert(&tx, p, *size, *mtime)?;
                }
                tx.commit()
            })?;
            batch.clear();
            Ok(())
        };
        for root in &scope.roots {
            let walker = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                let is_dir = e.file_type().is_dir();
                e.depth() == 0
                    || !(scope.skip_name(&name, is_dir)
                        || (e.depth() == 1 && is_dir && SKIP_TOP.contains(&name.as_ref())))
            });
            for entry in walker.filter_map(Result::ok).filter(|e| e.file_type().is_file()) {
                let Ok(meta) = entry.metadata() else { continue };
                let path = entry.path().display().to_string();
                let (size, m) = (meta.len(), mtime(&meta));
                stats.files += 1;
                if known.get(&path) != Some(&(size as i64, m)) {
                    batch.push((entry.into_path(), size, m));
                    stats.added_or_changed += 1;
                    if batch.len() >= 500 {
                        flush(&mut batch)?;
                    }
                }
                seen.insert(path);
            }
        }
        flush(&mut batch)?;
        // Invalidate: deleted files and anything no longer inside the allowed folders.
        let gone: Vec<&String> = known.keys().filter(|p| !seen.contains(*p)).collect();
        stats.removed = gone.len();
        self.store.with(|c| {
            let tx = c.transaction()?;
            for p in gone {
                remove(&tx, p)?;
            }
            tx.commit()
        })?;
        // After a big clean-up (folders removed from the allowed list), give the space back.
        if stats.removed > 10_000 && stats.removed * 5 > known.len() {
            let _ = self.store.with(|c| c.execute_batch("VACUUM;"));
        }
        self.store.set_meta("files_scanned", &chrono::Utc::now().timestamp().to_string());
        tracing::info!(
            "file index: {} files, {} new or changed, {} removed",
            stats.files,
            stats.added_or_changed,
            stats.removed
        );
        Ok(stats)
    }

    /// Re-check one path now (a watcher event, or a file tool changed it).
    pub fn refresh_path(&self, path: &Path) {
        let scope = self.scope();
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let _ = self.store.with(|c| {
            let tx = c.transaction()?;
            match std::fs::metadata(&canonical) {
                Ok(meta) if meta.is_file() && scope.contains(&canonical) => {
                    upsert(&tx, &canonical, meta.len(), mtime(&meta))?
                }
                Ok(meta) if meta.is_dir() => {}
                _ => {
                    // Gone (or moved/hidden): drop it, and everything under it if it was a folder.
                    let p = path.display().to_string();
                    remove(&tx, &p)?;
                    let prefix = format!("{p}{}", std::path::MAIN_SEPARATOR);
                    let ids: Vec<String> = {
                        let mut st = tx.prepare("SELECT path FROM files WHERE path >= ?1 AND path < ?2")?;
                        let end = format!("{p}{}", (std::path::MAIN_SEPARATOR as u8 + 1) as char);
                        st.query_map([&prefix, &end], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
                    };
                    for child in ids {
                        remove(&tx, &child)?;
                    }
                }
            }
            tx.commit()
        });
    }

    /// Files whose name, folder or (documents) text match `query`, best first. Each hit is
    /// re-checked on disk: deleted files are dropped from the index, changed ones re-indexed.
    pub fn search_files(&self, query: &str, limit: usize) -> Vec<FileHit> {
        let mut out = vec![];
        for any in [false, true] {
            let Some(q) = fts_query(query, any) else { return out };
            let rows: Vec<(String, i64, i64, bool)> = self
                .store
                .with(|c| {
                    let mut st = c.prepare(
                        "SELECT f.path, f.size, f.mtime, f.has_text
                         FROM files_fts JOIN files f ON f.id = files_fts.rowid
                         WHERE files_fts MATCH ?1 ORDER BY bm25(files_fts, 10.0, 3.0, 1.0) LIMIT ?2",
                    )?;
                    st.query_map(params![q, (limit * 3) as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                        .collect()
                })
                .unwrap_or_default();
            for (path, size, m, has_text) in rows {
                let p = PathBuf::from(&path);
                match std::fs::metadata(&p) {
                    Err(_) => self.refresh_path(&p), // gone: invalidate
                    Ok(meta) => {
                        if meta.len() as i64 != size || mtime(&meta) != m {
                            self.refresh_path(&p); // changed since indexed
                        }
                        let snippet = if has_text { snippet_of(&p, meta.len(), query) } else { String::new() };
                        out.push(FileHit { path, modified: when(mtime(&meta)), size: meta.len(), snippet });
                    }
                }
                if out.len() >= limit {
                    return out;
                }
            }
            if !out.is_empty() {
                break;
            }
        }
        out
    }

    // --- capabilities ------------------------------------------------------------------------
    /// Replace the capability index with `caps` if it changed. Returns whether it did.
    pub fn sync_capabilities(&self, caps: &[Capability]) -> bool {
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for c in caps {
                (&c.kind, &c.name, &c.source, &c.description).hash(&mut h);
            }
            h.finish().to_string()
        };
        if self.store.meta("caps_fingerprint").as_deref() == Some(fingerprint.as_str()) {
            return false;
        }
        let ok = self
            .store
            .with(|c| {
                let tx = c.transaction()?;
                tx.execute("DELETE FROM caps", [])?;
                tx.execute("DELETE FROM caps_fts", [])?;
                for cap in caps {
                    let key = format!("{}:{}", cap.kind, cap.name);
                    tx.execute(
                        "INSERT OR REPLACE INTO caps(key, kind, name, source, description) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![key, cap.kind, cap.name, cap.source, cap.description],
                    )?;
                    let id = tx.last_insert_rowid();
                    tx.execute(
                        "INSERT INTO caps_fts(rowid, name, source, description) VALUES (?1, ?2, ?3, ?4)",
                        params![id, cap.name.replace(['_', '-'], " "), cap.source, cap.description],
                    )?;
                }
                tx.commit()
            })
            .is_ok();
        if ok {
            self.store.set_meta("caps_fingerprint", &fingerprint);
            tracing::info!("capability index: {} entries", caps.len());
        }
        ok
    }

    pub fn capabilities(&self) -> Vec<Capability> {
        self.store
            .with(|c| {
                let mut st = c.prepare("SELECT kind, name, source, description FROM caps ORDER BY id")?;
                st.query_map([], |r| {
                    Ok(Capability { kind: r.get(0)?, name: r.get(1)?, source: r.get(2)?, description: r.get(3)? })
                })?
                .collect()
            })
            .unwrap_or_default()
    }

    /// Capabilities that look relevant to a task, best first.
    pub fn find_capabilities(&self, task: &str, limit: usize) -> Vec<Capability> {
        for any in [false, true] {
            let Some(q) = fts_query(task, any) else { return vec![] };
            let found: Vec<Capability> = self
                .store
                .with(|c| {
                    let mut st = c.prepare(
                        "SELECT c.kind, c.name, c.source, c.description FROM caps_fts JOIN caps c ON c.id = caps_fts.rowid
                         WHERE caps_fts MATCH ?1 ORDER BY bm25(caps_fts, 5.0, 2.0, 1.0) LIMIT ?2",
                    )?;
                    st.query_map(params![q, limit as i64], |r| {
                        Ok(Capability { kind: r.get(0)?, name: r.get(1)?, source: r.get(2)?, description: r.get(3)? })
                    })?
                    .collect()
                })
                .unwrap_or_default();
            if !found.is_empty() {
                return found;
            }
        }
        vec![]
    }

    pub fn find_capabilities_json(&self, task: &str, limit: usize) -> Value {
        let found = self.find_capabilities(task, limit);
        if found.is_empty() {
            return json!("Nothing indexed matches that. The available tools are listed in your instructions.");
        }
        json!(found)
    }
}

/// Watch the allowed folders and re-check paths as they change. Best effort: when the OS runs
/// out of watches (very large folders), the periodic scan and per-search checks still keep the
/// index accurate.
pub fn watch(index: Arc<Index>, rt: tokio::runtime::Handle) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && !matches!(event.kind, notify::EventKind::Access(_))
        {
            for p in event.paths {
                let _ = tx.send(p);
            }
        }
    })
    .ok()?;
    let mut watched = 0;
    for root in index.scope().roots {
        match watcher.watch(&root, RecursiveMode::Recursive) {
            Ok(()) => watched += 1,
            Err(e) => tracing::info!("can't watch all of {} ({e}); relying on periodic re-scans there", root.display()),
        }
    }
    let idx = index.clone();
    rt.spawn(async move {
        // Debounce bursts (an editor saving, a download finishing).
        loop {
            let Some(first) = rx.recv().await else { return };
            let mut paths: HashSet<PathBuf> = HashSet::from([first]);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(800);
            while let Ok(Some(p)) = tokio::time::timeout_at(deadline, rx.recv()).await {
                paths.insert(p);
            }
            let idx = idx.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let scope = idx.scope();
                for p in paths {
                    let name_ok = p.file_name().map(|n| !n.to_string_lossy().starts_with('.')).unwrap_or(false);
                    if name_ok && scope.roots.iter().any(|r| p.starts_with(r)) {
                        idx.refresh_path(&p);
                    }
                }
            })
            .await;
        }
    });
    (watched > 0).then_some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(name: &str) -> (PathBuf, Arc<Index>) {
        let dir = std::env::temp_dir().join(format!("fane-index-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Documents/Invoices")).unwrap();
        std::fs::create_dir_all(dir.join(".secret")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(dir.join("anaconda3/pkgs")).unwrap();
        std::fs::write(dir.join("anaconda3/pkgs/podcasts.md"), "tool install").unwrap();
        std::fs::write(dir.join("Documents/Budget 2026.md"), "Podcasts: $3,000\nSearch: $15,000\n").unwrap();
        std::fs::write(dir.join("Documents/Invoices/invoice-1042.pdf"), [0u8, 1, 2]).unwrap();
        std::fs::write(dir.join(".secret/podcasts.md"), "hidden podcasts").unwrap();
        std::fs::write(dir.join("node_modules/x/podcasts.md"), "generated").unwrap();
        std::fs::write(dir.join("Documents/keys.pem"), "podcasts").unwrap();
        let cfg = FilesCfg { roots: vec![dir.display().to_string()], ..Default::default() };
        let idx = Arc::new(Index::new(Arc::new(Store::in_memory().unwrap()), Scope::from_cfg(&cfg)));
        (dir.canonicalize().unwrap(), idx)
    }

    #[test]
    fn indexes_names_and_document_text_but_not_hidden_or_blocked() {
        let (dir, idx) = setup("basic");
        let s = idx.scan().unwrap();
        assert_eq!(s.files, 2, "{s:?}");
        let hits = idx.search_files("podcasts", 10);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].path.ends_with("Budget 2026.md") && hits[0].snippet.contains("3,000"));
        assert!(idx.search_files("invoices", 10)[0].path.ends_with("invoice-1042.pdf"), "folder names match");
        assert!(idx.search_files("1042", 10)[0].path.contains("invoice-1042"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalidates_changed_and_deleted_files() {
        let (dir, idx) = setup("invalidate");
        idx.scan().unwrap();
        // Changed on disk after indexing: a re-scan picks it up.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(dir.join("Documents/Budget 2026.md"), "Podcasts: cancelled\n").unwrap();
        let s = idx.scan().unwrap();
        assert_eq!((s.added_or_changed, s.removed), (1, 0));
        assert!(idx.search_files("cancelled", 5).len() == 1);
        assert!(idx.search_files("3,000", 5).is_empty(), "old text is gone");
        // Deleted: a search re-checks and drops it even before the next scan.
        std::fs::remove_file(dir.join("Documents/Budget 2026.md")).unwrap();
        assert!(idx.search_files("cancelled", 5).is_empty());
        assert_eq!(idx.file_count(), 1);
        // A folder removed: refresh_path drops everything under it.
        std::fs::remove_dir_all(dir.join("Documents/Invoices")).unwrap();
        idx.refresh_path(&dir.join("Documents/Invoices"));
        assert_eq!(idx.file_count(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn narrowing_the_scope_drops_files_outside_it() {
        let (dir, idx) = setup("scope");
        idx.scan().unwrap();
        let cfg = FilesCfg { roots: vec![dir.join("Documents/Invoices").display().to_string()], ..Default::default() };
        idx.set_scope(Scope::from_cfg(&cfg));
        let s = idx.scan().unwrap();
        assert_eq!((s.files, s.removed), (1, 1));
        assert!(idx.search_files("podcasts", 5).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capabilities_are_searchable_and_replaced_on_change() {
        let idx = Index::new(Arc::new(Store::in_memory().unwrap()), Scope { roots: vec![], blocked: vec![] });
        let cap = |n: &str, s: &str, d: &str| Capability {
            kind: "tool".into(),
            name: n.into(),
            source: s.into(),
            description: d.into(),
        };
        let v1 = vec![
            cap("calendar__list_events", "MCP server calendar", "List calendar events for a day"),
            cap("fane_send_email", "Fane's email", "Send an email from Fane's own address"),
        ];
        assert!(idx.sync_capabilities(&v1));
        assert!(!idx.sync_capabilities(&v1), "unchanged: nothing to do");
        assert_eq!(idx.find_capabilities("what's on my calendar tomorrow", 3)[0].name, "calendar__list_events");
        assert_eq!(idx.find_capabilities("send email", 3)[0].name, "fane_send_email");
        // The calendar server was removed: its tools disappear from the index.
        assert!(idx.sync_capabilities(&v1[1..]));
        assert!(idx.find_capabilities("calendar events", 3).iter().all(|c| !c.name.starts_with("calendar")));
    }
}
