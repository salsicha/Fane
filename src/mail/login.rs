//! Signing Fane in to its own mailbox with an address and an app password, on any system:
//! server settings for well-known providers, and the password kept in the system keychain
//! (macOS Keychain, Windows Credential Manager, the Secret Service on Linux), never in the
//! config file.

/// Server settings for a provider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    pub provider: &'static str,
    pub imap_host: &'static str,
    pub smtp_host: &'static str,
    pub smtp_port: u16,
    /// The server that stamps incoming mail with its verdict (Authentication-Results), as an
    /// exact name or "*.domain". Empty: not known; Fane learns it from a message you send.
    pub auth_server: &'static str,
    /// Where to make an app password.
    pub app_password_help: &'static str,
}

const PRESETS: &[(&[&str], Preset)] = &[
    (
        &["gmail.com", "googlemail.com"],
        Preset {
            provider: "Gmail",
            imap_host: "imap.gmail.com",
            smtp_host: "smtp.gmail.com",
            smtp_port: 587,
            auth_server: "mx.google.com",
            app_password_help: "https://myaccount.google.com/apppasswords (needs 2-Step Verification)",
        },
    ),
    (
        &["fastmail.com", "fastmail.fm"],
        Preset {
            provider: "Fastmail",
            imap_host: "imap.fastmail.com",
            smtp_host: "smtp.fastmail.com",
            smtp_port: 587,
            auth_server: "*.messagingengine.com",
            app_password_help: "Fastmail → Settings → Privacy & Security → app passwords",
        },
    ),
    (
        &["icloud.com", "me.com", "mac.com"],
        Preset {
            provider: "iCloud Mail",
            imap_host: "imap.mail.me.com",
            smtp_host: "smtp.mail.me.com",
            smtp_port: 587,
            auth_server: "",
            app_password_help: "https://account.apple.com → Sign-In and Security → App-Specific Passwords",
        },
    ),
    (
        &["yahoo.com", "ymail.com"],
        Preset {
            provider: "Yahoo Mail",
            imap_host: "imap.mail.yahoo.com",
            smtp_host: "smtp.mail.yahoo.com",
            smtp_port: 587,
            auth_server: "",
            app_password_help: "Yahoo → Account security → Generate app password",
        },
    ),
];

/// Providers that only allow their own sign-in for other apps (no app passwords).
const OAUTH_ONLY: &[&str] = &["outlook.com", "hotmail.com", "live.com", "msn.com"];

pub fn domain(address: &str) -> String {
    address.rsplit_once('@').map(|(_, d)| d.trim().to_lowercase()).unwrap_or_default()
}

pub fn preset(address: &str) -> Option<Preset> {
    let d = domain(address);
    PRESETS.iter().find(|(domains, _)| domains.contains(&d.as_str())).map(|(_, p)| *p)
}

/// Why this address can't sign in with a password, if it can't.
pub fn unsupported(address: &str) -> Option<String> {
    let d = domain(address);
    OAUTH_ONLY.contains(&d.as_str()).then(|| {
        format!("{d} doesn't accept app passwords for other mail programs, so Fane can't sign in to it yet; use a Gmail, Fastmail or iCloud address for Fane")
    })
}

/// Sign-in details from Settings or `fane mail login`. Empty servers mean the provider's.
#[derive(Debug, Clone, Default)]
pub struct Login {
    pub address: String,
    pub password: String,
    pub imap_host: String,
    pub smtp_host: String,
    pub smtp_port: u16,
}

/// Check that the address and app password work on both servers, then keep the password in
/// the keychain. Returns a note for the user.
pub async fn sign_in(login: &Login) -> Result<String, String> {
    let address = login.address.trim().to_lowercase();
    if !crate::core::valid_email(&address) {
        return Err(format!("{address} doesn't look like an email address"));
    }
    if let Some(why) = unsupported(&address) {
        return Err(why);
    }
    let preset = preset(&address);
    let imap = if login.imap_host.trim().is_empty() {
        preset.map(|p| p.imap_host).unwrap_or("")
    } else {
        login.imap_host.trim()
    };
    let smtp = if login.smtp_host.trim().is_empty() {
        preset.map(|p| p.smtp_host).unwrap_or("")
    } else {
        login.smtp_host.trim()
    };
    if imap.is_empty() || smtp.is_empty() {
        return Err(format!("Fane doesn't know {}'s mail servers; enter them (IMAP and SMTP)", domain(&address)));
    }
    let port = if login.smtp_port != 0 { login.smtp_port } else { preset.map_or(587, |p| p.smtp_port) };
    let password: String = login.password.chars().filter(|c| !c.is_whitespace()).collect(); // "abcd efgh ijkl mnop"
    let client = super::gmail::ImapClient::new(
        &address,
        super::gmail::Auth::Password(password.clone()),
        (imap, 993),
        (smtp, port),
    );
    client.check().await.map_err(|e| e.to_string())?;
    save_password(&address, &password)?;
    let verifier = preset.map(|p| p.auth_server).unwrap_or("");
    Ok(if verifier.is_empty() {
        format!(
            "Signed in as {address}. To let Fane act on your email, send it a message from your own address, then click Learn (or run `fane mail learn`)"
        )
    } else {
        format!("Signed in as {address}")
    })
}

const SERVICE: &str = "fane-mail";

fn entry(address: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &address.trim().to_lowercase()).map_err(|e| format!("system keychain: {e}"))
}

/// Run a keychain call on its own thread: the Linux backend blocks on its own async runtime,
/// which can't happen on one of Fane's runtime threads.
fn off_runtime<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(f).join().expect("keychain thread")
}

pub fn save_password(address: &str, password: &str) -> Result<(), String> {
    let (address, password) = (address.to_string(), password.to_string());
    off_runtime(move || {
        entry(&address)?
            .set_password(&password)
            .map_err(|e| format!("couldn't save the password in the system keychain: {e}"))
    })
}

/// The saved password, if any (None also when the keychain isn't available).
pub fn load_password(address: &str) -> Option<String> {
    let address = address.to_string();
    off_runtime(move || entry(&address).ok()?.get_password().ok())
}

pub fn delete_password(address: &str) {
    let address = address.to_string();
    off_runtime(move || {
        if let Ok(e) = entry(&address) {
            let _ = e.delete_credential();
        }
    })
}

/// Whether an Authentication-Results server name matches a configured pattern: exactly, or
/// "*.example.com" for any server under example.com.
pub fn server_matches(server: &str, pattern: &str) -> bool {
    let server = server.trim().to_lowercase();
    let pattern = pattern.trim().to_lowercase();
    if server.is_empty() || pattern.is_empty() {
        return false;
    }
    match pattern.strip_prefix("*.") {
        Some(base) => server.ends_with(&format!(".{base}")),
        None => server == pattern,
    }
}

/// The name of the server that wrote an Authentication-Results header (the part before ';').
pub fn authserv_id(auth_results: &str) -> String {
    auth_results.split(';').next().unwrap_or("").split_whitespace().next().unwrap_or("").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_unsupported() {
        assert_eq!(preset("Fane.Agent@GMail.com").unwrap().imap_host, "imap.gmail.com");
        assert_eq!(preset("me@fastmail.fm").unwrap().auth_server, "*.messagingengine.com");
        assert_eq!(preset("me@example.org"), None);
        assert!(unsupported("me@outlook.com").is_some());
        assert!(unsupported("me@gmail.com").is_none());
    }

    /// Talks to the real system keychain, so run by hand: `cargo test keychain -- --ignored`.
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let addr = format!("fane-test-{}@example.invalid", std::process::id());
        save_password(&addr, "abcd efgh").unwrap();
        assert_eq!(load_password(&addr).as_deref(), Some("abcd efgh"));
        delete_password(&addr);
        assert_eq!(load_password(&addr), None);
    }

    /// Same, from inside the async runtime (where Fane calls it).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn keychain_inside_tokio() {
        let addr = format!("fane-test-async-{}@example.invalid", std::process::id());
        save_password(&addr, "pw").unwrap();
        assert_eq!(load_password(&addr).as_deref(), Some("pw"));
        delete_password(&addr);
    }

    #[test]
    fn server_patterns() {
        assert!(server_matches("mx.google.com", "mx.google.com"));
        assert!(server_matches("MX3.messagingengine.com", "*.messagingengine.com"));
        assert!(!server_matches("messagingengine.com.evil.example", "*.messagingengine.com"));
        assert!(!server_matches("evilmessagingengine.com", "*.messagingengine.com"));
        assert!(!server_matches("mx.google.com", ""));
        assert_eq!(authserv_id("mx.google.com; dkim=pass header.i=@gmail.com"), "mx.google.com");
        assert_eq!(authserv_id("  mx01.mail.icloud.com 1; dmarc=pass"), "mx01.mail.icloud.com");
    }
}
