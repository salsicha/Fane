//! Read recent mail from local Thunderbird profiles (mbox stores), read-only.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use mail_parser::{AddressList, DateTime, MessageParser, PartKind};
use regex::Regex;
use serde_json::{Value, json};

use crate::config::expand;

const TAIL_BYTES: u64 = 4 * 1024 * 1024;
const INBOX_NAMES: [&str; 3] = ["inbox", "INBOX", "Inbox"];
const BODY_MAX: usize = 4000;
const PREVIEW_MAX: usize = 240;

static MBOX_SEP: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| regex::bytes::Regex::new(r"(?-u)(?:^|\n)From [^\n]*\n").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// One parsed message; serialized as {from, to, subject, date, account, body}.
#[derive(Debug, Clone)]
struct Mail {
    from: String,
    to: String,
    subject: String,
    date: String,
    /// Seconds since the epoch, for sorting; `None` when the date didn't parse.
    timestamp: Option<i64>,
    account: String,
    body: String,
}

impl Mail {
    fn to_json(&self, include_body: bool) -> Value {
        let body: String =
            if include_body { self.body.clone() } else { self.body.chars().take(PREVIEW_MAX).collect() };
        json!({"from": self.from, "to": self.to, "subject": self.subject, "date": self.date,
               "account": self.account, "body": body})
    }
}

pub struct ThunderbirdMail {
    /// Profile roots that exist, after `config::expand`.
    pub roots: Vec<PathBuf>,
}

impl ThunderbirdMail {
    pub fn new(profile_roots: &[String]) -> Self {
        Self { roots: profile_roots.iter().map(|r| expand(r)).filter(|p| p.is_dir()).collect() }
    }

    /// Inbox mbox files: `<root>/<profile>/{Mail,ImapMail}/<account>/<Inbox|INBOX|inbox>`.
    pub fn mailboxes(&self) -> Vec<PathBuf> {
        let mut boxes = Vec::new();
        for root in &self.roots {
            for profile in subdirs(root) {
                for store in ["Mail", "ImapMail"] {
                    for account in subdirs(&profile.join(store)) {
                        let Ok(rd) = std::fs::read_dir(&account) else { continue };
                        let mut found: Vec<PathBuf> = rd
                            .flatten()
                            .filter(|e| INBOX_NAMES.iter().any(|n| e.file_name() == *n))
                            .map(|e| e.path())
                            .filter(|p| p.is_file())
                            .collect();
                        found.sort();
                        boxes.extend(found);
                    }
                }
            }
        }
        boxes
    }

    /// [{from, to, subject, date, account, body}] newest first, or a hint string.
    pub async fn recent(&self, limit: usize, query: &str, include_body: bool) -> anyhow::Result<Value> {
        if self.roots.is_empty() {
            return Ok(Value::String(
                "No Thunderbird profile found. Connect your mail through an MCP server instead.".into(),
            ));
        }
        let boxes = self.mailboxes();
        let query = query.to_string();
        let msgs = tokio::task::spawn_blocking(move || recent_from(&boxes, limit, &query)).await?;
        if msgs.is_empty() {
            return Ok(Value::String("No messages found.".into()));
        }
        Ok(Value::Array(msgs.iter().map(|m| m.to_json(include_body)).collect()))
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    out.sort();
    out
}

fn recent_from(boxes: &[PathBuf], limit: usize, query: &str) -> Vec<Mail> {
    let mut msgs: Vec<Mail> = boxes.iter().filter_map(|b| read_tail(b).ok()).flatten().collect();
    if !query.is_empty() {
        let q = query.to_lowercase();
        msgs.retain(|m| format!("{}{}{}", m.subject, m.from, m.body).to_lowercase().contains(&q));
    }
    msgs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.date.cmp(&a.date)));
    msgs.truncate(limit);
    msgs
}

/// Parse the messages in the last `TAIL_BYTES` of an mbox file.
fn read_tail(mbox: &Path) -> std::io::Result<Vec<Mail>> {
    let mut f = std::fs::File::open(mbox)?;
    let size = f.metadata()?.len();
    f.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))?;
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;
    let account = mbox.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(parse_mbox(&data, size > TAIL_BYTES, &account))
}

fn parse_mbox(data: &[u8], truncated: bool, account: &str) -> Vec<Mail> {
    let mut chunks: Vec<&[u8]> = MBOX_SEP.split(data).collect();
    if truncated && !chunks.is_empty() {
        chunks.remove(0); // first chunk is probably a partial message
    }
    let parser = MessageParser::default();
    chunks
        .into_iter()
        .filter(|raw| !raw.trim_ascii().is_empty())
        .filter_map(|raw| parse_message(&parser, raw, account))
        .collect()
}

fn parse_message(parser: &MessageParser, raw: &[u8], account: &str) -> Option<Mail> {
    let msg = parser.parse(raw)?;
    let subject = msg.subject().unwrap_or_default().to_string();
    let from = msg.from().map(format_addresses).unwrap_or_default();
    if subject.is_empty() && from.is_empty() {
        return None;
    }
    let to = msg.to().map(format_addresses).unwrap_or_default();

    // text/plain preferred, else HTML (tags are stripped below either way, like the original).
    let plain = msg.text_body().find(|p| matches!(p.kind(), PartKind::Text)).and_then(|p| p.text());
    let text = plain
        .or_else(|| msg.html_body().chain(msg.text_body()).find(|p| matches!(p.kind(), PartKind::Html)).and_then(|p| p.text()))
        .unwrap_or_default();
    let text = TAG.replace_all(&text, " ");
    let body: String = SPACE.replace_all(&text, " ").trim().chars().take(BODY_MAX).collect();

    let (date, timestamp) = match msg.date().filter(DateTime::is_valid) {
        Some(d) => (iso_date(&d), Some(d.to_timestamp())),
        None => {
            let raw = msg.headers().get("Date").map(|h| String::from_utf8_lossy(h.raw_value()).trim().to_string());
            (raw.unwrap_or_default(), None)
        }
    };
    Some(Mail { from, to, subject, date, timestamp, account: account.to_string(), body })
}

/// Like Python's `datetime.isoformat()`: `2024-01-05T10:00:00+01:00`.
fn iso_date(d: &DateTime) -> String {
    let sign = if d.tz_before_gmt && (d.tz_hour > 0 || d.tz_minute > 0) { '-' } else { '+' };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        d.year, d.month, d.day, d.hour, d.minute, d.second, d.tz_hour, d.tz_minute
    )
}

/// "Name <addr>, other@example.com" (groups flattened).
fn format_addresses(list: AddressList<'_>) -> String {
    list.mailboxes()
        .map(|m| match (m.name(), m.address()) {
            (Some(n), Some(a)) if !n.is_empty() => format!("{n} <{a}>"),
            (_, Some(a)) => a.to_string(),
            (Some(n), None) => n.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const MBOX: &str = "From - Mon Jan  1 00:00:00 2024\n\
From: Alice Smith <alice@example.com>\n\
To: me@example.com\n\
Subject: Lunch plans\n\
Date: Mon, 01 Jan 2024 12:00:00 +0000\n\
\n\
Want to grab lunch tomorrow?\n\
\n\
From - Tue Jan  2 00:00:00 2024\n\
From: News <news@example.org>\n\
To: me@example.com\n\
Subject: Weekly digest\n\
Date: Tue, 02 Jan 2024 09:30:00 +0100\n\
Content-Type: text/html; charset=utf-8\n\
\n\
<html><body><h1>Top   stories</h1><p>Rust 2024 is out.</p></body></html>\n\
\n\
From - Wed Jan  3 00:00:00 2024\n\
From: bob@example.com\n\
To: me@example.com\n\
Subject: Report\n\
Date: Wed, 03 Jan 2024 08:00:00 -0500\n\
Content-Type: multipart/alternative; boundary=\"b\"\n\
\n\
--b\n\
Content-Type: text/plain\n\
\n\
PLAINBODY report attached LONG\n\
--b\n\
Content-Type: text/html\n\
\n\
<p>HTMLBODY</p>\n\
--b--\n";

    fn fake_profile(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("fane-tb-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let acct = root.join("abcd.default-release/ImapMail/imap.example.com");
        fs::create_dir_all(&acct).unwrap();
        let body = MBOX.replace("LONG", &"x".repeat(300));
        fs::write(acct.join("INBOX"), body).unwrap();
        fs::write(acct.join("Sent"), MBOX).unwrap();
        root
    }

    #[tokio::test]
    async fn reads_profile_inbox() {
        let root = fake_profile("read");
        let tb = ThunderbirdMail::new(&[root.display().to_string(), "/no/such/profile".into()]);
        assert_eq!(tb.roots.len(), 1);
        assert_eq!(tb.mailboxes().len(), 1);

        let v = tb.recent(10, "", false).await.unwrap();
        let msgs = v.as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        // Newest first.
        assert_eq!(msgs[0]["subject"], "Report");
        assert_eq!(msgs[1]["subject"], "Weekly digest");
        assert_eq!(msgs[2]["subject"], "Lunch plans");
        assert_eq!(msgs[0]["date"], "2024-01-03T08:00:00-05:00");
        assert_eq!(msgs[2]["date"], "2024-01-01T12:00:00+00:00");
        assert_eq!(msgs[2]["from"], "Alice Smith <alice@example.com>");
        assert_eq!(msgs[0]["account"], "imap.example.com");
        // Plain part preferred; HTML stripped and whitespace collapsed.
        assert!(msgs[0]["body"].as_str().unwrap().starts_with("PLAINBODY report attached"));
        assert_eq!(msgs[1]["body"], "Top stories Rust 2024 is out.");
        assert_eq!(msgs[2]["body"], "Want to grab lunch tomorrow?");
        // Preview truncation vs full body.
        assert_eq!(msgs[0]["body"].as_str().unwrap().chars().count(), 240);
        let full = tb.recent(1, "", true).await.unwrap();
        assert!(full[0]["body"].as_str().unwrap().chars().count() > 300);

        // Query filter (case-insensitive over subject+from+body) and limit.
        let q = tb.recent(10, "RUST", false).await.unwrap();
        assert_eq!(q.as_array().unwrap().len(), 1);
        assert_eq!(q[0]["subject"], "Weekly digest");
        assert_eq!(tb.recent(10, "alice@", false).await.unwrap()[0]["subject"], "Lunch plans");
        assert_eq!(tb.recent(2, "", false).await.unwrap().as_array().unwrap().len(), 2);
        assert_eq!(tb.recent(10, "nothing-matches", false).await.unwrap(), "No messages found.");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn truncated_tail_drops_first_chunk() {
        // The tail starts mid-message: that partial chunk is dropped only when truncated.
        let tail = &MBOX[MBOX.find("Subject: Lunch").unwrap()..];
        assert_eq!(parse_mbox(tail.as_bytes(), false, "a").len(), 3);
        let msgs = parse_mbox(tail.as_bytes(), true, "a");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].subject, "Weekly digest");
        assert_eq!(parse_mbox(MBOX.as_bytes(), true, "a").len(), 3);
        assert!(parse_mbox(b"", false, "a").is_empty());
    }

    #[tokio::test]
    async fn no_profile_hint() {
        let tb = ThunderbirdMail::new(&[]);
        let v = tb.recent(10, "", false).await.unwrap();
        assert!(v.as_str().unwrap().starts_with("No Thunderbird profile found."));
    }
}
