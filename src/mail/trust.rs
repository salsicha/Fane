//! Deciding whether an email really came from the owner.
//!
//! A From: header is trivially forged, so trust requires Gmail's own verdict: the topmost
//! Authentication-Results header (added by mx.google.com on delivery) must show a DMARC
//! pass for the sender's domain. Only the owner's address is ever trusted.
// Not yet reachable from main until the integrator wires the mailbox in.

use std::sync::LazyLock;

use regex::Regex;

use super::gmail::MailMessage;

static RE_DMARC_PASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bdmarc=pass\b").unwrap());

/// Gmail verified the From: domain with DMARC.
pub fn dmarc_pass(msg: &MailMessage) -> bool {
    let ar = msg.auth_results.trim();
    if !ar.to_lowercase().starts_with("mx.google.com") {
        return false;
    }
    let Some((_, domain)) = msg.from_addr.rsplit_once('@') else {
        return false;
    };
    let domain = domain.to_lowercase();
    if domain.is_empty() {
        return false;
    }
    let header_from =
        Regex::new(&format!(r"(?i)\bheader\.from={}(?:[\s;)]|$)", regex::escape(&domain))).expect("escaped regex");
    RE_DMARC_PASS.is_match(ar) && header_from.is_match(ar)
}

/// The owner's address if the message is from the owner *and* authenticated, else None.
pub fn verified_owner(msg: &MailMessage, owner: Option<&str>) -> Option<String> {
    let owner = owner?.trim().to_lowercase();
    if owner.is_empty() || msg.from_addr.is_empty() || msg.from_addr != owner || !dmarc_pass(msg) {
        return None;
    }
    Some(owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::gmail::parse_message;
    use crate::mail::gmail::tests::{GENUINE_AR, USER, raw};

    #[test]
    fn genuine_owner() {
        let m = parse_message(1, &raw(USER, "Hi", "Please check my notes", GENUINE_AR, ""), "");
        assert!(dmarc_pass(&m));
        assert_eq!(verified_owner(&m, Some(USER)).as_deref(), Some(USER));
        // Owner given in mixed case (config) still matches.
        assert_eq!(verified_owner(&m, Some("Salsicha@Gmail.com")).as_deref(), Some(USER));
    }

    #[test]
    fn spoofed_from_is_rejected() {
        // Forged From: with Gmail's real verdict showing DMARC failure.
        let bad = "mx.google.com; spf=fail smtp.mailfrom=evil.example; dmarc=fail (p=NONE) header.from=gmail.com";
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", bad, ""), ""), Some(USER)), None);
    }

    #[test]
    fn attacker_supplied_auth_header_below_gmails_is_ignored() {
        // Gmail's own header is topmost; a fake "pass" further down must not count.
        let real = "mx.google.com; dmarc=fail header.from=gmail.com";
        let m = parse_message(1, &raw(USER, "Hi", "x", real, &format!("Authentication-Results: {GENUINE_AR}")), "");
        assert_eq!(verified_owner(&m, Some(USER)), None);
    }

    #[test]
    fn missing_or_foreign_auth_header() {
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", "", ""), ""), Some(USER)), None);
        let other = GENUINE_AR.replace("mx.google.com", "mx.evil.example");
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", &other, ""), ""), Some(USER)), None);
        // DMARC passed, but for a different domain than the From: address.
        let wrong = GENUINE_AR.replace("header.from=gmail.com", "header.from=evil.example");
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", &wrong, ""), ""), Some(USER)), None);
        // The domain must match exactly, not as a prefix.
        for d in ["gmail.co", "gmail.com.evil.example", "gmail.com-evil.example"] {
            let ar = GENUINE_AR.replace("header.from=gmail.com", &format!("header.from={d}"));
            assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", &ar, ""), ""), Some(USER)), None, "{d}");
        }
    }

    #[test]
    fn other_sender_with_valid_dmarc_is_not_the_owner() {
        let ar = GENUINE_AR.replace("salsicha@gmail.com", "dana@gmail.com");
        let m = parse_message(1, &raw("dana@gmail.com", "Hi", "x", &ar, ""), "");
        assert!(dmarc_pass(&m));
        assert_eq!(verified_owner(&m, Some(USER)), None);
    }

    #[test]
    fn no_owner_configured() {
        let m = parse_message(1, &raw(USER, "Hi", "x", GENUINE_AR, ""), "");
        assert_eq!(verified_owner(&m, None), None);
        assert_eq!(verified_owner(&m, Some("  ")), None);
    }
}
