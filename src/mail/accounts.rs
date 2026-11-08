//! GNOME Online Accounts (Settings → Online Accounts): the Google sign-in built into Ubuntu.
//!
//! Fane never stores Google credentials. It asks GOA for a short-lived OAuth token for the
//! one account the user selected, and only that account.
// Not yet reachable from main until the integrator wires the mailbox in.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct AccountError(pub String);

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GoaAccount {
    pub path: String,
    pub provider: String,
    pub email: String,
    pub name: String,
    pub mail_disabled: bool,
    pub attention_needed: bool,
    pub imap_host: String,
    pub smtp_host: String,
}

#[cfg(not(target_os = "linux"))]
const NOT_LINUX: &str = "Fane's own email uses GNOME Online Accounts, which is only available on Linux.";

/// Client for GNOME Online Accounts. Share it behind an `Arc`; the D-Bus connection is
/// opened lazily and tokens are cached per account.
#[derive(Default)]
pub struct OnlineAccounts {
    #[cfg(target_os = "linux")]
    conn: tokio::sync::OnceCell<zbus::Connection>,
    /// account path → (token, expiry)
    tokens: Mutex<HashMap<String, (String, Instant)>>,
}

impl OnlineAccounts {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn find(&self, email: &str) -> Result<Option<GoaAccount>, AccountError> {
        let email = email.trim().to_lowercase();
        Ok(self.google_accounts().await?.into_iter().find(|a| a.email.to_lowercase() == email))
    }

    /// Drop the cached token, e.g. after the server rejected it.
    pub fn forget(&self, acc: &GoaAccount) {
        self.tokens.lock().unwrap().remove(&acc.path);
    }

    fn cached_token(&self, acc: &GoaAccount) -> Option<String> {
        let tokens = self.tokens.lock().unwrap();
        let (token, expiry) = tokens.get(&acc.path)?;
        (*expiry > Instant::now() + std::time::Duration::from_secs(60)).then(|| token.clone())
    }
}

#[cfg(not(target_os = "linux"))]
impl OnlineAccounts {
    /// Google accounts that expose mail, sorted by address. Errors off Linux / without GOA.
    pub async fn google_accounts(&self) -> Result<Vec<GoaAccount>, AccountError> {
        Err(AccountError(NOT_LINUX.into()))
    }

    /// Short-lived OAuth2 access token (cached until a minute before expiry).
    pub async fn access_token(&self, _acc: &GoaAccount) -> Result<String, AccountError> {
        Err(AccountError(NOT_LINUX.into()))
    }
}

#[cfg(target_os = "linux")]
mod goa {
    use std::collections::HashMap;

    use zbus::{OwnedObjectPath, OwnedVariant, Variant};

    pub const GOA: &str = "org.gnome.OnlineAccounts";
    pub const GOA_PATH: &str = "/org/gnome/OnlineAccounts";

    pub type Props = HashMap<String, OwnedVariant>;
    pub type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, Props>>;

    fn prop_str(props: &Props, key: &str) -> Option<String> {
        match props.get(key).map(|v| &**v) {
            Some(Variant::Str(s)) => Some(s.to_string()).filter(|s| !s.is_empty()),
            _ => None,
        }
    }

    fn prop_bool(props: &Props, key: &str) -> bool {
        matches!(props.get(key).map(|v| &**v), Some(Variant::Bool(true)))
    }

    /// A Google account with a Mail interface, from one GetManagedObjects entry.
    pub fn account_from(path: &str, ifaces: &HashMap<String, Props>) -> Option<super::GoaAccount> {
        let acc = ifaces.get(&format!("{GOA}.Account"))?;
        let mail = ifaces.get(&format!("{GOA}.Mail"))?;
        if prop_str(acc, "ProviderType").as_deref() != Some("google") {
            return None;
        }
        Some(super::GoaAccount {
            path: path.to_string(),
            provider: "google".into(),
            email: prop_str(mail, "EmailAddress").or_else(|| prop_str(acc, "PresentationIdentity")).unwrap_or_default(),
            name: prop_str(mail, "Name").unwrap_or_default(),
            mail_disabled: prop_bool(acc, "MailDisabled"),
            attention_needed: prop_bool(acc, "AttentionNeeded"),
            imap_host: prop_str(mail, "ImapHost").unwrap_or_else(|| "imap.gmail.com".into()),
            smtp_host: prop_str(mail, "SmtpHost").unwrap_or_else(|| "smtp.gmail.com".into()),
        })
    }
}

#[cfg(target_os = "linux")]
impl OnlineAccounts {
    async fn conn(&self) -> Result<&zbus::Connection, AccountError> {
        self.conn
            .get_or_try_init(zbus::Connection::session)
            .await
            .map_err(|e| AccountError(format!("cannot reach the session bus: {e}")))
    }

    async fn call<R>(&self, path: &str, interface: &str, member: &str) -> Result<R, AccountError>
    where
        R: for<'d> zbus::wire::DynamicDeserialize<'d>,
    {
        let conn = self.conn().await?;
        let reply = conn
            .call_method(Some(goa::GOA), path, Some(interface), member, &())
            .await
            .map_err(|e| AccountError(format!("{member}: {e}")))?;
        reply.body().deserialize().map_err(|e| AccountError(format!("{member}: unexpected reply: {e}")))
    }

    /// Google accounts that expose mail, sorted by address. Errors off Linux / without GOA.
    pub async fn google_accounts(&self) -> Result<Vec<GoaAccount>, AccountError> {
        let objects: goa::ManagedObjects = self
            .call(goa::GOA_PATH, "org.freedesktop.DBus.ObjectManager", "GetManagedObjects")
            .await
            .map_err(|e| AccountError(format!("GNOME Online Accounts unavailable: {e}")))?;
        let mut accounts: Vec<GoaAccount> =
            objects.iter().filter_map(|(path, ifaces)| goa::account_from(path.as_str(), ifaces)).collect();
        accounts.sort_by_key(|a| a.email.to_lowercase());
        Ok(accounts)
    }

    /// Short-lived OAuth2 access token (cached until a minute before expiry).
    pub async fn access_token(&self, acc: &GoaAccount) -> Result<String, AccountError> {
        if let Some(token) = self.cached_token(acc) {
            return Ok(token);
        }
        if acc.mail_disabled {
            return Err(AccountError(format!("Mail is switched off for {} in Settings → Online Accounts", acc.email)));
        }
        let attention = |e: AccountError| AccountError(format!("{} needs attention in Settings → Online Accounts ({e})", acc.email));
        let _: (i32,) = self.call(&acc.path, &format!("{}.Account", goa::GOA), "EnsureCredentials").await.map_err(attention)?;
        let (token, expires_in): (String, i32) =
            self.call(&acc.path, &format!("{}.OAuth2Based", goa::GOA), "GetAccessToken").await.map_err(attention)?;
        let lifetime = if expires_in > 0 { expires_in as u64 } else { 600 };
        let expiry = Instant::now() + std::time::Duration::from_secs(lifetime);
        self.tokens.lock().unwrap().insert(acc.path.clone(), (token.clone(), expiry));
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> GoaAccount {
        GoaAccount {
            path: "/org/gnome/OnlineAccounts/Accounts/account_1".into(),
            provider: "google".into(),
            email: "fane.agent@gmail.com".into(),
            name: "Fane".into(),
            mail_disabled: true,
            attention_needed: false,
            imap_host: "imap.gmail.com".into(),
            smtp_host: "smtp.gmail.com".into(),
        }
    }

    #[test]
    fn token_cache_expiry() {
        let oa = OnlineAccounts::new();
        let acc = account();
        let now = Instant::now();
        oa.tokens.lock().unwrap().insert(acc.path.clone(), ("fresh".into(), now + std::time::Duration::from_secs(600)));
        assert_eq!(oa.cached_token(&acc).as_deref(), Some("fresh"));
        oa.tokens.lock().unwrap().insert(acc.path.clone(), ("stale".into(), now + std::time::Duration::from_secs(30)));
        assert_eq!(oa.cached_token(&acc), None);
        oa.forget(&acc);
        assert!(oa.tokens.lock().unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn mail_disabled_is_an_error() {
        let err = OnlineAccounts::new().access_token(&account()).await.unwrap_err();
        assert!(err.0.contains("Mail is switched off"), "{err}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_managed_objects() {
        use zbus::{OwnedVariant, Str};
        let s = |v: &str| OwnedVariant::from(Str::from(v.to_string()));
        let props = |kv: Vec<(&str, OwnedVariant)>| -> goa::Props { kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect() };
        let mut ifaces = HashMap::new();
        ifaces.insert(
            "org.gnome.OnlineAccounts.Account".to_string(),
            props(vec![("ProviderType", s("google")), ("PresentationIdentity", s("fane.agent@gmail.com")),
                       ("MailDisabled", OwnedVariant::from(false)), ("AttentionNeeded", OwnedVariant::from(true))]),
        );
        ifaces.insert("org.gnome.OnlineAccounts.Mail".to_string(), props(vec![("Name", s("Fane")), ("ImapHost", s(""))]));
        let acc = goa::account_from("/a/1", &ifaces).unwrap();
        assert_eq!(acc.email, "fane.agent@gmail.com");
        assert_eq!(acc.name, "Fane");
        assert!(!acc.mail_disabled && acc.attention_needed);
        assert_eq!(acc.imap_host, "imap.gmail.com");

        ifaces.remove("org.gnome.OnlineAccounts.Mail");
        assert!(goa::account_from("/a/1", &ifaces).is_none());
    }
}
