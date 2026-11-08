//! GNOME Online Accounts (Settings → Online Accounts): the Google sign-in built into Ubuntu.
// STUB: replaced by the mail port.

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct AccountError(pub String);

#[derive(Debug, Clone, serde::Serialize)]
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

#[derive(Default)]
pub struct OnlineAccounts;

impl OnlineAccounts {
    pub fn new() -> Self {
        OnlineAccounts
    }

    /// Google accounts that expose mail, sorted by address. Errors off Linux / without GOA.
    pub async fn google_accounts(&self) -> Result<Vec<GoaAccount>, AccountError> {
        Err(AccountError("not implemented".into()))
    }

    pub async fn find(&self, _email: &str) -> Result<Option<GoaAccount>, AccountError> {
        Ok(None)
    }

    /// Short-lived OAuth2 access token (cached until a minute before expiry).
    pub async fn access_token(&self, _acc: &GoaAccount) -> Result<String, AccountError> {
        Err(AccountError("not implemented".into()))
    }
}
