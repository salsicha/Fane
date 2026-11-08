//! Fane's own mailbox: watch for new mail and answer the owner's emailed requests.
// STUB: replaced by the mail port.

use std::sync::Arc;

use async_trait::async_trait;

use crate::tools::Tool;

/// What the mailbox needs from the rest of Fane.
#[async_trait]
pub trait MailHost: Send + Sync {
    /// Run an emailed request through the agent (reduced tool set); returns the reply text.
    async fn run_remote(&self, prompt: &str) -> String;
    fn notify(&self, text: &str);
    /// Ask the user (Send/Cancel card) before mail goes to anyone but the owner.
    async fn confirm_send(&self, details: serde_json::Value) -> bool;
}

pub struct AgentMailbox {
    pub address: String,
}

impl AgentMailbox {
    /// Connect to `address` in GNOME Online Accounts and start polling. Errors are user-facing.
    pub async fn connect(
        _accounts: Arc<super::accounts::OnlineAccounts>,
        _cfg: crate::config::AgentMailCfg,
        _host: Arc<dyn MailHost>,
    ) -> anyhow::Result<Arc<AgentMailbox>> {
        anyhow::bail!("not implemented")
    }

    pub async fn stop(&self) {}

    /// Last poll error, if the mailbox is currently failing.
    pub fn error(&self) -> Option<String> {
        None
    }

    /// fane_inbox, fane_read_email, fane_send_email, fane_reply_email.
    pub fn tools(self: &Arc<Self>) -> Vec<Tool> {
        vec![]
    }
}

pub fn mail_tool_names() -> [&'static str; 4] {
    ["fane_inbox", "fane_read_email", "fane_send_email", "fane_reply_email"]
}
