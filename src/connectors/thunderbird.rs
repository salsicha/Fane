//! Read recent mail from local Thunderbird profiles (mbox stores), read-only.
// STUB: replaced by the connectors port.

use serde_json::Value;

pub struct ThunderbirdMail;

impl ThunderbirdMail {
    pub fn new(_profile_roots: &[String]) -> Self {
        ThunderbirdMail
    }

    /// [{from, to, subject, date, account, body}] newest first, or a hint string.
    pub async fn recent(&self, _limit: usize, _query: &str, _include_body: bool) -> anyhow::Result<Value> {
        Ok(Value::String("No Thunderbird profile found.".into()))
    }
}
