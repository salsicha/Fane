//! Gmail over IMAP/SMTP with XOAUTH2, using tokens from GNOME Online Accounts.
// Not yet reachable from main until the integrator wires the mailbox in.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_imap::types::{Fetch, Flag};
use async_trait::async_trait;
use futures::TryStreamExt;
use futures::future::BoxFuture;
use lettre::message::Mailbox as LettreMailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use mail_parser::{MessageParser, PartKind};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;

const MAX_BODY_CHARS: usize = 20_000;
const MAX_FETCH: usize = 50;
const NET_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound for a whole IMAP operation (connect, auth, commands, logout).
const OP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("IMAP: {0}")]
    Imap(String),
    #[error("SMTP: {0}")]
    Smtp(String),
    /// Getting an OAuth token failed (e.g. the account needs attention in Online Accounts).
    #[error("{0}")]
    Auth(String),
    /// The outgoing message couldn't be built (bad address, ...).
    #[error("{0}")]
    Message(String),
}

/// Produces a fresh (or cached) OAuth2 access token for the account.
pub type TokenFn = Arc<dyn Fn() -> BoxFuture<'static, Result<String, MailError>> + Send + Sync>;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MailMessage {
    pub uid: u32,
    /// With angle brackets, e.g. `<abc@mail.gmail.com>`.
    pub message_id: String,
    /// Display form of the sender, e.g. `Alex <alex@example.com>`.
    pub from: String,
    /// The sender's address, lowercased. Empty when the From header is ambiguous
    /// (several From headers or several addresses), so such mail is never trusted.
    pub from_addr: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    /// ISO-8601 if parseable, else the raw header.
    pub date: String,
    pub body: String,
    pub seen: bool,
    /// Space-separated message ids, each in angle brackets.
    pub references: String,
    /// Topmost Authentication-Results header, i.e. the one the provider's servers added.
    pub auth_results: String,
}

impl MailMessage {
    /// Minutes since the Date header (None if it can't be read).
    pub fn age_minutes(&self) -> Option<i64> {
        let sent = chrono::DateTime::parse_from_rfc3339(&self.date).ok()?;
        Some((chrono::Utc::now() - sent.with_timezone(&chrono::Utc)).num_minutes())
    }

    pub fn summary(&self, snippet: usize) -> Value {
        let collapsed = self.body.split_whitespace().collect::<Vec<_>>().join(" ");
        json!({"uid": self.uid, "from": self.from, "subject": self.subject, "date": self.date,
               "unread": !self.seen, "snippet": truncate_chars(&collapsed, snippet)})
    }
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn bracketed(id: &str) -> String {
    let id = id.trim();
    if id.is_empty() || id.starts_with('<') { id.to_string() } else { format!("<{id}>") }
}

static RE_SCRIPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").unwrap());
static RE_STYLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").unwrap());
static RE_BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>|</p\s*>").unwrap());
static RE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static RE_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+").unwrap());
static RE_WROTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*On .{5,200} wrote:\s*$").unwrap());

/// HTML to rough plain text: drop scripts/styles, line breaks for <br> and </p>, strip tags.
fn html_to_plain(html: &str) -> String {
    let s = RE_SCRIPT.replace_all(html, " ");
    let s = RE_STYLE.replace_all(&s, " ");
    let s = RE_BREAK.replace_all(&s, "\n");
    let s = RE_TAG.replace_all(&s, " ");
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Parse a raw RFC 5322 message. `flags` is the IMAP flag list, e.g. `\Seen \Answered`.
pub fn parse_message(uid: u32, raw: &[u8], flags: &str) -> MailMessage {
    let seen = flags.split_whitespace().any(|f| f.eq_ignore_ascii_case("\\Seen"));
    let Some(msg) = MessageParser::new().parse(raw) else {
        return MailMessage { uid, seen, ..Default::default() };
    };
    let headers = msg.headers();

    // Plain text preferred, HTML as a fallback (attachments are in neither list).
    let body = if let Some(text) = msg.text_body().find(|p| matches!(p.kind(), PartKind::Text)).and_then(|p| p.text()) {
        text.into_owned()
    } else if let Some(html) = msg.html_body().find(|p| matches!(p.kind(), PartKind::Html)).and_then(|p| p.text()) {
        html_to_plain(&html)
    } else {
        String::new()
    };
    let body = body.replace("\r\n", "\n");
    let body = RE_SPACES.replace_all(&body, " ").trim().to_string();

    // The first From header; ambiguous senders get no address at all (fail closed).
    let from_headers: Vec<_> = headers.all("From").collect();
    let first_from = from_headers.first().and_then(|h| h.value().as_address());
    let mailboxes: Vec<_> = first_from.map(|l| l.mailboxes().collect()).unwrap_or_default();
    let (from, mut from_addr) = match mailboxes.first() {
        Some(mb) => {
            let addr = mb.address().unwrap_or("").to_string();
            let display = match mb.name() {
                Some(name) if !name.is_empty() => format!("{name} <{addr}>"),
                _ => addr.clone(),
            };
            (display, addr.to_lowercase())
        }
        None => (String::new(), String::new()),
    };
    if from_headers.len() > 1 || mailboxes.len() > 1 {
        from_addr.clear();
    }

    let addresses = |name: &str| -> Vec<String> {
        headers
            .all(name)
            .filter_map(|h| h.value().as_address())
            .flat_map(|l| l.mailboxes().filter_map(|m| m.address().map(str::to_string)).collect::<Vec<_>>())
            .filter(|a| !a.is_empty())
            .collect()
    };

    let date = match headers.all("Date").next() {
        Some(h) => match h.value().as_datetime().filter(|d| d.is_valid()) {
            Some(d) => d.to_rfc3339(),
            None => String::from_utf8_lossy(h.raw_value()).trim().to_string(),
        },
        None => String::new(),
    };

    let auth_results = headers
        .all("Authentication-Results")
        .next()
        .map(|h| String::from_utf8_lossy(h.raw_value()).split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();

    let references = headers
        .all("References")
        .next()
        .and_then(|h| h.value().as_text_list())
        .map(|l| l.iter().map(bracketed).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();

    MailMessage {
        uid,
        message_id: headers
            .all("Message-ID")
            .next()
            .and_then(|h| h.value().as_text())
            .map(bracketed)
            .unwrap_or_default(),
        from,
        from_addr,
        to: addresses("To"),
        cc: addresses("Cc"),
        subject: headers.all("Subject").next().and_then(|h| h.value().as_text()).unwrap_or("").to_string(),
        date,
        body: truncate_chars(&body, MAX_BODY_CHARS),
        seen,
        references,
        auth_results,
    }
}

/// Drop the quoted history under a reply ("On … wrote:" and "> " lines).
pub fn strip_quoted(body: &str) -> String {
    let mut out = Vec::new();
    for line in body.lines() {
        if RE_WROTE.is_match(line) || line.trim() == "-----Original Message-----" {
            break;
        }
        if !line.trim_start().starts_with('>') {
            out.push(line);
        }
    }
    out.join("\n").trim().to_string()
}

fn make_msgid(domain: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("<{nanos:x}.{}.{n}@{domain}>", std::process::id())
}

fn mailbox(addr: &str) -> Result<LettreMailbox, MailError> {
    addr.trim().parse().map_err(|e| MailError::Message(format!("invalid address {addr:?}: {e}")))
}

/// Build an outgoing plain-text message. Replying to `reply_to` threads it (In-Reply-To,
/// References) and defaults the subject to "Re: <original subject>".
pub fn build_message(
    sender: &str,
    to: &[String],
    subject: &str,
    body: &str,
    cc: &[String],
    reply_to: Option<&MailMessage>,
) -> Result<lettre::Message, MailError> {
    let mut b = lettre::Message::builder().from(mailbox(sender)?);
    for a in to {
        b = b.to(mailbox(a)?);
    }
    for a in cc {
        b = b.cc(mailbox(a)?);
    }
    let mut subject = subject.to_string();
    if let Some(orig) = reply_to {
        if subject.is_empty() {
            subject = orig.subject.clone();
        }
        if !subject.to_lowercase().starts_with("re:") {
            subject = format!("Re: {subject}");
        }
        if !orig.message_id.is_empty() {
            b = b
                .in_reply_to(orig.message_id.clone())
                .references(format!("{} {}", orig.references, orig.message_id).trim().to_string());
        }
    }
    let domain = sender.rsplit('@').next().unwrap_or("localhost").trim();
    b.subject(subject)
        .date_now()
        .message_id(Some(make_msgid(domain)))
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .map_err(|e| MailError::Message(e.to_string()))
}

/// Fane's own mailbox, as seen by the rest of the app (and faked in tests).
#[async_trait]
pub trait MailClient: Send + Sync {
    fn address(&self) -> &str;
    /// (UIDVALIDITY, highest UID) of the inbox.
    async fn state(&self) -> Result<(u32, u32), MailError>;
    /// Messages with UID > `last_uid` (at most 50, oldest first), plus the current UIDVALIDITY.
    async fn new_since(&self, last_uid: u32) -> Result<(u32, Vec<MailMessage>), MailError>;
    /// Newest first. A non-empty `query` is a Gmail search on Gmail, else words in the message.
    async fn inbox(&self, limit: usize, query: &str, unread_only: bool) -> Result<Vec<MailMessage>, MailError>;
    async fn get(&self, uid: u32) -> Result<Option<MailMessage>, MailError>;
    async fn mark_seen(&self, uid: u32) -> Result<(), MailError>;
    /// Send and return the Message-ID.
    async fn send(&self, message: lettre::Message) -> Result<String, MailError>;
}

fn xoauth2(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}

/// SASL XOAUTH2. On failure Gmail sends a JSON challenge, which must be answered with an
/// empty response before it reports the error.
struct XOAuth2 {
    response: String,
    sent: bool,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> String {
        if self.sent {
            String::new()
        } else {
            self.sent = true;
            self.response.clone()
        }
    }
}

/// IMAP flags as text, e.g. `\Seen \Flagged custom`.
pub fn flags_text<'a>(flags: impl IntoIterator<Item = Flag<'a>>) -> String {
    flags
        .into_iter()
        .map(|f| match f {
            Flag::Seen => "\\Seen".to_string(),
            Flag::Answered => "\\Answered".to_string(),
            Flag::Flagged => "\\Flagged".to_string(),
            Flag::Deleted => "\\Deleted".to_string(),
            Flag::Draft => "\\Draft".to_string(),
            Flag::Recent => "\\Recent".to_string(),
            Flag::MayCreate => "\\*".to_string(),
            Flag::Custom(c) => c.into_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One FETCH response item (UID, FLAGS, BODY[]) to a message. Items without a UID or body
/// (e.g. unsolicited flag updates) are skipped.
pub fn message_from_fetch<'a>(
    uid: Option<u32>,
    flags: impl IntoIterator<Item = Flag<'a>>,
    body: Option<&[u8]>,
) -> Option<MailMessage> {
    Some(parse_message(uid?, body?, &flags_text(flags)))
}

type ImapSession = async_imap::Session<TlsStream<TcpStream>>;

fn imap_err(e: impl std::fmt::Display) -> MailError {
    MailError::Imap(e.to_string())
}

fn tls_connector() -> Result<TlsConnector, MailError> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    // Several crypto backends are compiled in (via other crates), so pick one explicitly
    // rather than relying on a process-wide default.
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(imap_err)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// How the client signs in.
#[derive(Clone)]
pub enum Auth {
    /// SASL XOAUTH2 with a fresh token for each connection (GNOME Online Accounts).
    OAuth(TokenFn),
    /// The account address and an app password.
    Password(String),
}

/// A mailbox over IMAP (TLS) and SMTP (STARTTLS on 587, or TLS on 465): Gmail through Online
/// Accounts, or any provider with an app password. One IMAP connection per operation: simple,
/// and immune to token expiry mid-session.
pub struct ImapClient {
    address: String,
    auth: Auth,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    lock: Mutex<()>,
}

impl ImapClient {
    pub fn new(address: &str, auth: Auth, imap: (&str, u16), smtp: (&str, u16)) -> Self {
        let or = |h: &str, d: &str| if h.trim().is_empty() { d.to_string() } else { h.trim().to_string() };
        let port = |p: u16, d: u16| if p == 0 { d } else { p };
        Self {
            address: address.to_string(),
            auth,
            imap_host: or(imap.0, "imap.gmail.com"),
            imap_port: port(imap.1, 993),
            smtp_host: or(smtp.0, "smtp.gmail.com"),
            smtp_port: port(smtp.1, 587),
            lock: Mutex::new(()),
        }
    }

    /// Gmail understands its own search syntax (X-GM-RAW); other servers get IMAP TEXT.
    fn gmail(&self) -> bool {
        self.imap_host.ends_with("gmail.com") || self.imap_host.ends_with("googlemail.com")
    }

    /// Check that it can sign in to both servers.
    pub async fn check(&self) -> Result<(), MailError> {
        self.state().await?;
        self.smtp().await?.test_connection().await.map_err(|e| MailError::Smtp(e.to_string()))?;
        Ok(())
    }

    async fn smtp(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>, MailError> {
        let (secret, mechanisms) = match &self.auth {
            Auth::OAuth(token) => (token().await?, vec![Mechanism::Xoauth2]),
            Auth::Password(p) => (p.clone(), vec![Mechanism::Plain, Mechanism::Login]),
        };
        let builder = if self.smtp_port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.smtp_host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.smtp_host)
        }
        .map_err(|e| MailError::Smtp(e.to_string()))?;
        Ok(builder
            .port(self.smtp_port)
            .credentials(Credentials::new(self.address.clone(), secret))
            .authentication(mechanisms)
            .timeout(Some(NET_TIMEOUT))
            .build())
    }

    /// Connect, authenticate and open INBOX (read-only unless `writable`). Returns UIDVALIDITY.
    async fn open(&self, writable: bool) -> Result<(ImapSession, u32), MailError> {
        let host = self.imap_host.as_str();
        let tcp = tokio::time::timeout(NET_TIMEOUT, TcpStream::connect((host, self.imap_port)))
            .await
            .map_err(|_| MailError::Imap(format!("timed out connecting to {host}")))?
            .map_err(imap_err)?;
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(imap_err)?;
        let tls = tls_connector()?.connect(name, tcp).await.map_err(imap_err)?;
        let mut client = async_imap::Client::new(tls);
        client
            .read_response()
            .await
            .map_err(imap_err)?
            .ok_or_else(|| MailError::Imap("connection closed before greeting".into()))?;
        let mut session = match &self.auth {
            Auth::OAuth(token) => {
                let token = token().await?;
                let auth = XOAuth2 { response: xoauth2(&self.address, &token), sent: false };
                client.authenticate("XOAUTH2", auth).await.map_err(|(e, _)| imap_err(e))?
            }
            Auth::Password(p) => client
                .login(&self.address, p)
                .await
                .map_err(|(e, _)| MailError::Auth(format!("{host} didn't accept the address and app password: {e}")))?,
        };
        let mailbox = if writable { session.select("INBOX").await } else { session.examine("INBOX").await }
            .map_err(|e| MailError::Imap(format!("cannot open INBOX: {e}")))?;
        Ok((session, mailbox.uid_validity.unwrap_or(0)))
    }

    async fn search(session: &mut ImapSession, query: &str) -> Result<Vec<u32>, MailError> {
        let mut uids: Vec<u32> = session.uid_search(query).await.map_err(imap_err)?.into_iter().collect();
        uids.sort_unstable();
        Ok(uids)
    }

    async fn fetch(session: &mut ImapSession, uids: &[u32]) -> Result<Vec<MailMessage>, MailError> {
        if uids.is_empty() {
            return Ok(vec![]);
        }
        let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let fetches: Vec<Fetch> = session
            .uid_fetch(set, "(UID FLAGS BODY.PEEK[])")
            .await
            .map_err(imap_err)?
            .try_collect()
            .await
            .map_err(imap_err)?;
        Ok(fetches.iter().filter_map(|f| message_from_fetch(f.uid, f.flags(), f.body())).collect())
    }

    /// Run one IMAP operation under the lock, with an overall timeout, logging out afterwards.
    async fn with_session<T, F>(&self, writable: bool, op: F) -> Result<T, MailError>
    where
        F: for<'s> FnOnce(&'s mut ImapSession, u32) -> BoxFuture<'s, Result<T, MailError>>,
    {
        let _guard = self.lock.lock().await;
        tokio::time::timeout(OP_TIMEOUT, async {
            let (mut session, validity) = self.open(writable).await?;
            let result = op(&mut session, validity).await;
            let _ = session.logout().await;
            result
        })
        .await
        .map_err(|_| MailError::Imap("timed out".into()))?
    }
}

#[async_trait]
impl MailClient for ImapClient {
    fn address(&self) -> &str {
        &self.address
    }

    async fn state(&self) -> Result<(u32, u32), MailError> {
        self.with_session(false, |s, validity| {
            Box::pin(async move {
                let uids = Self::search(s, "ALL").await?;
                Ok((validity, uids.last().copied().unwrap_or(0)))
            })
        })
        .await
    }

    async fn new_since(&self, last_uid: u32) -> Result<(u32, Vec<MailMessage>), MailError> {
        self.with_session(false, |s, validity| {
            Box::pin(async move {
                // "N:*" always matches the newest message, even an older one; filter below.
                let uids: Vec<u32> = Self::search(s, &format!("UID {}:*", last_uid.saturating_add(1)))
                    .await?
                    .into_iter()
                    .filter(|&u| u > last_uid)
                    .take(MAX_FETCH)
                    .collect();
                let mut msgs = Self::fetch(s, &uids).await?;
                msgs.sort_by_key(|m| m.uid);
                Ok((validity, msgs))
            })
        })
        .await
    }

    async fn inbox(&self, limit: usize, query: &str, unread_only: bool) -> Result<Vec<MailMessage>, MailError> {
        let mut criteria = if unread_only { "UNSEEN".to_string() } else { "ALL".to_string() };
        let query: String = query.chars().filter(|c| !matches!(c, '"' | '\\' | '\r' | '\n')).collect();
        let mut query = query.trim().to_string();
        if !self.gmail() {
            // Gmail's "from:x" becomes IMAP FROM; the rest is searched as text.
            if let Some(rest) = query.strip_prefix("from:") {
                let (from, rest) = rest.split_once(' ').unwrap_or((rest, ""));
                criteria = format!("{criteria} FROM \"{from}\"");
                query = rest.trim().to_string();
            }
        }
        if !query.is_empty() {
            let kind = if self.gmail() { "X-GM-RAW" } else { "TEXT" };
            criteria = format!("{criteria} {kind} \"{query}\"");
        }
        let limit = limit.clamp(1, MAX_FETCH);
        self.with_session(false, |s, _| {
            Box::pin(async move {
                let uids = Self::search(s, &criteria).await?;
                let newest = &uids[uids.len().saturating_sub(limit)..];
                let mut msgs = Self::fetch(s, newest).await?;
                msgs.sort_by_key(|m| std::cmp::Reverse(m.uid));
                Ok(msgs)
            })
        })
        .await
    }

    async fn get(&self, uid: u32) -> Result<Option<MailMessage>, MailError> {
        self.with_session(false, |s, _| {
            Box::pin(async move { Ok(Self::fetch(s, &[uid]).await?.into_iter().find(|m| m.uid == uid)) })
        })
        .await
    }

    async fn mark_seen(&self, uid: u32) -> Result<(), MailError> {
        self.with_session(true, |s, _| {
            Box::pin(async move {
                let _: Vec<Fetch> = s
                    .uid_store(uid.to_string(), "+FLAGS (\\Seen)")
                    .await
                    .map_err(imap_err)?
                    .try_collect()
                    .await
                    .map_err(imap_err)?;
                Ok(())
            })
        })
        .await
    }

    async fn send(&self, message: lettre::Message) -> Result<String, MailError> {
        let id = message.headers().get_raw("Message-ID").unwrap_or("").to_string();
        let transport = self.smtp().await?;
        let _guard = self.lock.lock().await;
        transport.send(message).await.map_err(|e| MailError::Smtp(e.to_string()))?;
        Ok(id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const ME: &str = "fane.agent@gmail.com";
    pub const USER: &str = "salsicha@gmail.com";
    pub const GENUINE_AR: &str = "mx.google.com; dkim=pass header.i=@gmail.com header.s=20230601; \
        spf=pass smtp.mailfrom=salsicha@gmail.com; dmarc=pass (p=NONE sp=QUARANTINE) header.from=gmail.com";

    /// A raw message as Gmail would deliver it.
    pub fn raw(from: &str, subject: &str, body: &str, auth: &str, extra: &str) -> Vec<u8> {
        let mut lines: Vec<String> = vec![];
        if !auth.is_empty() {
            lines.push(format!("Authentication-Results: {auth}"));
        }
        lines.extend(extra.lines().map(str::to_string));
        lines.extend([
            format!("From: Alex <{from}>"),
            format!("To: {ME}"),
            format!("Subject: {subject}"),
            "Date: Sat, 07 Nov 2026 09:00:00 -0800".into(),
            "Message-ID: <abc@mail.gmail.com>".into(),
            String::new(),
            body.into(),
        ]);
        lines.join("\r\n").into_bytes()
    }

    pub fn owner_raw(subject: &str, body: &str) -> Vec<u8> {
        raw(USER, subject, body, GENUINE_AR, "")
    }

    #[test]
    fn parses_headers() {
        let m = parse_message(1, &raw(USER, "Hi", "Please check my notes", GENUINE_AR, "Cc: a@x.com, B <b@y.com>"), "");
        assert_eq!(m.uid, 1);
        assert_eq!(m.from, format!("Alex <{USER}>"));
        assert_eq!(m.from_addr, USER);
        assert_eq!(m.to, vec![ME]);
        assert_eq!(m.cc, vec!["a@x.com", "b@y.com"]);
        assert_eq!(m.subject, "Hi");
        assert_eq!(m.date, "2026-11-07T09:00:00-08:00");
        assert_eq!(m.message_id, "<abc@mail.gmail.com>");
        assert_eq!(m.body, "Please check my notes");
        assert_eq!(m.auth_results, GENUINE_AR);
        assert!(!m.seen);
    }

    #[test]
    fn uppercase_sender_is_lowercased() {
        let m = parse_message(1, &raw("Salsicha@Gmail.COM", "Hi", "x", GENUINE_AR, ""), "");
        assert_eq!(m.from_addr, USER);
    }

    #[test]
    fn duplicate_from_is_ambiguous() {
        let m = parse_message(1, &raw("evil@example.com", "Hi", "x", GENUINE_AR, &format!("From: {USER}")), "");
        assert_eq!(m.from_addr, "");
    }

    #[test]
    fn html_body_and_quote_stripping() {
        let raw = b"From: a@b.com\r\nSubject: s\r\nContent-Type: text/html\r\n\r\n<p>Book the room</p><br><script>x()</script>";
        let m = parse_message(2, raw, "");
        assert!(m.body.contains("Book the room") && !m.body.contains("x()"), "{:?}", m.body);
        assert!(!m.body.contains('<'));
        assert_eq!(strip_quoted("Yes please\n\nOn Fri, Nov 6, 2026 at 9:00 AM Fane wrote:\n> old"), "Yes please");
        assert_eq!(strip_quoted("a\n> quoted\nb\n-----Original Message-----\nold"), "a\nb");
    }

    #[test]
    fn plain_text_preferred_over_html() {
        let raw = "From: a@b.com\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
            Content-Type: multipart/alternative; boundary=\"XX\"\r\n\r\n\
            --XX\r\nContent-Type: text/plain\r\n\r\nplain   version\r\n\
            --XX\r\nContent-Type: text/html\r\n\r\n<p>html version</p>\r\n--XX--\r\n";
        let m = parse_message(3, raw.as_bytes(), "");
        assert_eq!(m.body, "plain version");
    }

    #[test]
    fn body_is_capped() {
        let m = parse_message(1, &owner_raw("Hi", &"é".repeat(30_000)), "");
        assert_eq!(m.body.chars().count(), MAX_BODY_CHARS);
    }

    fn reparse(msg: &lettre::Message) -> MailMessage {
        parse_message(0, &msg.formatted(), "")
    }

    #[test]
    fn reply_threads_correctly() {
        let orig = parse_message(3, &owner_raw("Plans", "x"), "");
        let msg = build_message(ME, &[USER.into()], "", "Sure — done", &[], Some(&orig)).unwrap();
        let h = msg.headers();
        assert_eq!(h.get_raw("Subject"), Some("Re: Plans"));
        assert_eq!(h.get_raw("In-Reply-To"), Some("<abc@mail.gmail.com>"));
        assert!(h.get_raw("References").unwrap().contains("<abc@mail.gmail.com>"));
        assert!(h.get_raw("Message-ID").unwrap().ends_with("@gmail.com>"));
        assert!(h.get_raw("Date").is_some());
        let back = reparse(&msg);
        assert_eq!(back.body, "Sure — done");
        assert_eq!(back.to, vec![USER]);
        assert_eq!(back.subject, "Re: Plans");

        // Already "Re:" — not doubled; references accumulate.
        let mut orig2 = orig.clone();
        orig2.subject = "RE: Plans".into();
        orig2.references = "<older@x>".into();
        let msg = build_message(ME, &[USER.into()], "", "ok", &[], Some(&orig2)).unwrap();
        assert_eq!(msg.headers().get_raw("Subject"), Some("RE: Plans"));
        assert_eq!(reparse(&msg).references, "<older@x> <abc@mail.gmail.com>");
    }

    #[test]
    fn invalid_address_is_an_error() {
        assert!(matches!(
            build_message(ME, &["not an address".into()], "s", "b", &[], None),
            Err(MailError::Message(_))
        ));
    }

    #[test]
    fn imap_fetch_parsing() {
        let raw = owner_raw("Hi", "x");
        let m = message_from_fetch(Some(42), [Flag::Seen, Flag::Custom("$Label".into())], Some(&raw)).unwrap();
        assert!(m.uid == 42 && m.seen && m.subject == "Hi");
        assert!(message_from_fetch(None, [], Some(&raw)).is_none());
        assert!(message_from_fetch(Some(1), [Flag::Seen], None).is_none());
        assert!(!message_from_fetch(Some(1), [Flag::Flagged], Some(&raw)).unwrap().seen);
        assert_eq!(flags_text([Flag::Seen, Flag::Answered]), "\\Seen \\Answered");
    }

    #[test]
    fn xoauth2_authenticator() {
        use async_imap::Authenticator;
        let mut a = XOAuth2 { response: xoauth2(ME, "tok"), sent: false };
        assert_eq!(a.process(b""), format!("user={ME}\x01auth=Bearer tok\x01\x01"));
        assert_eq!(a.process(b"{\"status\":\"400\"}"), "");
    }

    #[test]
    fn summary_collapses_and_truncates() {
        let m = parse_message(7, &owner_raw("Hi", "a\n\nb   c"), "\\Seen");
        let s = m.summary(3);
        assert_eq!(s["snippet"], "a b");
        assert_eq!(s["unread"], false);
        assert_eq!(s["uid"], 7);
    }
}
