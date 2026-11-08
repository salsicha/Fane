//! Plain-text / Markdown notes folders (Obsidian vaults, ~/Notes, ...).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::config::{CONFIG_PATH, expand};

const NOTE_SUFFIXES: [&str; 4] = ["md", "markdown", "txt", "org"];
const READ_LIMIT: usize = 20_000;

pub struct Notes {
    /// Configured folders that exist, after `config::expand`.
    pub dirs: Vec<PathBuf>,
}

impl Notes {
    /// Keeps only the configured dirs that exist (after `config::expand`).
    pub fn new(dirs: &[String]) -> Self {
        Self { dirs: dirs.iter().map(|d| expand(d)).filter(|p| p.is_dir()).collect() }
    }

    /// Ranked hits: [{path, title, score, modified, snippet}], or a hint string if no folders.
    pub async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Value> {
        if self.dirs.is_empty() {
            return Ok(Value::String(format!(
                "No notes folders found. Set [connectors] notes_dirs in {}.",
                CONFIG_PATH.display()
            )));
        }
        let dirs = self.dirs.clone();
        let query = query.to_string();
        let hits = tokio::task::spawn_blocking(move || search_dirs(&dirs, &query, limit)).await?;
        Ok(Value::Array(hits))
    }

    /// Full text (first 20k chars) of a note inside a configured folder.
    pub async fn read(&self, path: &str) -> anyhow::Result<String> {
        let dirs = self.dirs.clone();
        let path = path.to_string();
        tokio::task::spawn_blocking(move || read_note(&dirs, &path)).await?
    }
}

fn is_note(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| NOTE_SUFFIXES.iter().any(|s| e.eq_ignore_ascii_case(s)))
}

/// Note files under `dirs`, skipping anything below a dot-file or dot-folder.
fn note_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in dirs {
        let walker = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'));
        for entry in walker.flatten() {
            let p = entry.path();
            if entry.depth() > 0 && is_note(p) && p.is_file() {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}

fn read_lossy(path: &Path) -> std::io::Result<String> {
    Ok(String::from_utf8_lossy(&std::fs::read(path)?).into_owned())
}

/// `text` from char index `start` (inclusive) to `end` (exclusive).
fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end.saturating_sub(start)).collect()
}

/// Score and snippet for one note, or `None` when it doesn't match.
fn score_note(text: &str, stem: &str, terms: &[String]) -> Option<(usize, String)> {
    let low = text.to_lowercase();
    let name = stem.to_lowercase();
    let score = if terms.is_empty() {
        1
    } else {
        terms.iter().map(|t| low.matches(t.as_str()).count() + if name.contains(t.as_str()) { 5 } else { 0 }).sum()
    };
    if score == 0 {
        return None;
    }
    let idx = terms
        .iter()
        .filter_map(|t| low.find(t.as_str()))
        .min()
        .map(|byte| low[..byte].chars().count())
        .unwrap_or(0);
    let snippet = char_slice(text, idx.saturating_sub(120), idx + 280).replace('\n', " ").trim().to_string();
    Some((score, snippet))
}

fn search_dirs(dirs: &[PathBuf], query: &str, limit: usize) -> Vec<Value> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(usize, f64, Value)> = Vec::new();
    for p in note_files(dirs) {
        let Ok(text) = read_lossy(&p) else { continue };
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Some((score, snippet)) = score_note(&text, &stem, &terms) else { continue };
        let modified = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        hits.push((
            score,
            modified,
            json!({"path": p.display().to_string(), "title": stem, "score": score,
                   "modified": modified, "snippet": snippet}),
        ));
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
    hits.into_iter().take(limit).map(|h| h.2).collect()
}

fn read_note(dirs: &[PathBuf], path: &str) -> anyhow::Result<String> {
    const OUTSIDE: &str = "error: path is outside the configured notes folders";
    let roots: Vec<PathBuf> = dirs.iter().map(|d| d.canonicalize().unwrap_or_else(|_| d.clone())).collect();
    let p = expand(path);
    let resolved = match p.canonicalize() {
        Ok(r) => r,
        // Missing file: report the IO error only for paths that would be inside a notes folder.
        Err(e) if roots.iter().chain(dirs).any(|d| p.starts_with(d)) => return Err(e.into()),
        Err(_) => return Ok(OUTSIDE.into()),
    };
    if !roots.iter().any(|d| resolved.starts_with(d)) {
        return Ok(OUTSIDE.into());
    }
    Ok(read_lossy(&resolved)?.chars().take(READ_LIMIT).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fane-notes-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn search_ranks_and_skips_hidden() {
        let d = tmpdir("search");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::create_dir_all(d.join(".obsidian")).unwrap();
        fs::write(d.join("groceries.md"), "milk\neggs\nbread").unwrap();
        fs::write(d.join("sub/Recipes.TXT"), "pancakes need milk and eggs and more milk").unwrap();
        fs::write(d.join(".obsidian/milk.md"), "milk milk milk milk").unwrap();
        fs::write(d.join("image.png"), "milk").unwrap();
        fs::write(d.join("other.org"), "nothing relevant").unwrap();

        let notes = Notes::new(&[d.display().to_string(), "/definitely/not/here".into()]);
        assert_eq!(notes.dirs.len(), 1);

        let hits = notes.search("Milk", 10).await.unwrap();
        let hits = hits.as_array().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["title"], "Recipes");
        assert_eq!(hits[0]["score"], 2);
        assert_eq!(hits[1]["title"], "groceries");
        assert_eq!(hits[1]["snippet"], "milk eggs bread");

        // Term in the file name weighs 5.
        let hits = notes.search("groceries", 10).await.unwrap();
        assert_eq!(hits[0]["score"], 5);

        // No terms: everything matches with score 1.
        let all = notes.search("", 10).await.unwrap();
        assert_eq!(all.as_array().unwrap().len(), 3);
        assert_eq!(notes.search("", 1).await.unwrap().as_array().unwrap().len(), 1);

        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn snippet_window() {
        let text = format!("{}NEEDLE{}", "a".repeat(200), "b\n".repeat(200));
        let (score, snippet) = score_note(&text, "x", &["needle".into()]).unwrap();
        assert_eq!(score, 1);
        assert!(snippet.starts_with(&"a".repeat(120)));
        assert_eq!(snippet.chars().count(), 400 - 1); // trailing space trimmed
        assert!(!snippet.contains('\n'));
        assert!(score_note("abc", "x", &["zzz".into()]).is_none());
    }

    #[tokio::test]
    async fn read_guards_path() {
        let d = tmpdir("read");
        let outside = tmpdir("read-outside");
        fs::write(d.join("a.md"), "x".repeat(25_000)).unwrap();
        fs::write(outside.join("secret.md"), "secret").unwrap();
        let notes = Notes::new(&[d.display().to_string()]);

        let text = notes.read(&d.join("a.md").display().to_string()).await.unwrap();
        assert_eq!(text.len(), 20_000);
        let sneaky = d.join("..").join(outside.file_name().unwrap()).join("secret.md");
        assert_eq!(
            notes.read(&sneaky.display().to_string()).await.unwrap(),
            "error: path is outside the configured notes folders"
        );
        assert!(notes.read("/nonexistent/x.md").await.unwrap().starts_with("error: path is outside"));
        assert!(notes.read(&d.join("missing.md").display().to_string()).await.is_err());

        fs::remove_dir_all(&d).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[tokio::test]
    async fn no_dirs_hint() {
        let notes = Notes::new(&[]);
        let v = notes.search("x", 5).await.unwrap();
        assert!(v.as_str().unwrap().contains("[connectors] notes_dirs"));
    }
}
