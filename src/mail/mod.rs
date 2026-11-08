//! Fane's own email: a Gmail account from GNOME Online Accounts (Linux), read over IMAP and
//! sent over SMTP with OAuth tokens. Only mail from the owner's address is acted on.

pub mod accounts;
pub mod gmail;
pub mod mailbox;
pub mod trust;
