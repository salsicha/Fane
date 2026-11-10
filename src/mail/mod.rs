//! Fane's own email: any provider's account with an app password (kept in the system
//! keychain), or on Linux a Google account from GNOME Online Accounts, read over IMAP and sent
//! over SMTP. Only verified mail from the owner's address is acted on.

pub mod accounts;
pub mod gmail;
pub mod login;
pub mod mailbox;
pub mod trust;
