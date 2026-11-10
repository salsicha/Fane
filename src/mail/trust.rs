//! Deciding whether an email really came from the owner.
//!
//! A From: header is trivially forged, so trust requires the receiving provider's own verdict:
//! the topmost Authentication-Results header, written by the provider's server on delivery
//! (`auth_server`, e.g. mx.google.com for Gmail), must show a DMARC pass for the sender's
//! domain. Only the owner's address is ever trusted. With no known `auth_server`, nothing is.

use std::sync::LazyLock;

use regex::Regex;

use super::gmail::MailMessage;
use super::login::{authserv_id, server_matches};

static RE_DMARC_PASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bdmarc=pass\b").unwrap());

/// The provider's server (`auth_server`) verified the From: domain with DMARC.
pub fn dmarc_pass(msg: &MailMessage, auth_server: &str) -> bool {
    let ar = msg.auth_results.trim();
    if !server_matches(&authserv_id(ar), auth_server) {
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
pub fn verified_owner(msg: &MailMessage, owner: Option<&str>, auth_server: &str) -> Option<String> {
    let owner = owner?.trim().to_lowercase();
    if owner.is_empty() || msg.from_addr.is_empty() || msg.from_addr != owner || !dmarc_pass(msg, auth_server) {
        return None;
    }
    Some(owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::gmail::parse_message;
    use crate::mail::gmail::tests::{GENUINE_AR, USER, raw};

    const G: &str = "mx.google.com";

    #[test]
    fn genuine_owner() {
        let m = parse_message(1, &raw(USER, "Hi", "Please check my notes", GENUINE_AR, ""), "");
        assert!(dmarc_pass(&m, G));
        assert_eq!(verified_owner(&m, Some(USER), G).as_deref(), Some(USER));
        // Owner given in mixed case (config) still matches.
        assert_eq!(verified_owner(&m, Some("Salsicha@Gmail.com"), G).as_deref(), Some(USER));
    }

    #[test]
    fn spoofed_from_is_rejected() {
        // Forged From: with Gmail's real verdict showing DMARC failure.
        let bad = "mx.google.com; spf=fail smtp.mailfrom=evil.example; dmarc=fail (p=NONE) header.from=gmail.com";
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", bad, ""), ""), Some(USER), G), None);
    }

    #[test]
    fn attacker_supplied_auth_header_below_gmails_is_ignored() {
        // Gmail's own header is topmost; a fake "pass" further down must not count.
        let real = "mx.google.com; dmarc=fail header.from=gmail.com";
        let m = parse_message(1, &raw(USER, "Hi", "x", real, &format!("Authentication-Results: {GENUINE_AR}")), "");
        assert_eq!(verified_owner(&m, Some(USER), G), None);
    }

    #[test]
    fn missing_or_foreign_auth_header() {
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", "", ""), ""), Some(USER), G), None);
        let other = GENUINE_AR.replace("mx.google.com", "mx.evil.example");
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", &other, ""), ""), Some(USER), G), None);
        // DMARC passed, but for a different domain than the From: address.
        let wrong = GENUINE_AR.replace("header.from=gmail.com", "header.from=evil.example");
        assert_eq!(verified_owner(&parse_message(1, &raw(USER, "Hi", "x", &wrong, ""), ""), Some(USER), G), None);
        // The domain must match exactly, not as a prefix.
        for d in ["gmail.co", "gmail.com.evil.example", "gmail.com-evil.example"] {
            let ar = GENUINE_AR.replace("header.from=gmail.com", &format!("header.from={d}"));
            assert_eq!(
                verified_owner(&parse_message(1, &raw(USER, "Hi", "x", &ar, ""), ""), Some(USER), G),
                None,
                "{d}"
            );
        }
    }

    #[test]
    fn other_sender_with_valid_dmarc_is_not_the_owner() {
        let ar = GENUINE_AR.replace("salsicha@gmail.com", "dana@gmail.com");
        let m = parse_message(1, &raw("dana@gmail.com", "Hi", "x", &ar, ""), "");
        assert!(dmarc_pass(&m, G));
        assert_eq!(verified_owner(&m, Some(USER), G), None);
    }

    #[test]
    fn other_providers_and_unknown_verifier() {
        let fm = "mx4.messagingengine.com; dkim=pass; spf=pass; dmarc=pass (p=none) header.from=example.org";
        let m = parse_message(1, &raw("me@example.org", "Hi", "x", fm, ""), "");
        assert!(verified_owner(&m, Some("me@example.org"), "*.messagingengine.com").is_some());
        assert_eq!(verified_owner(&m, Some("me@example.org"), "mx.google.com"), None, "not that provider's verdict");
        assert_eq!(verified_owner(&m, Some("me@example.org"), ""), None, "no known verifier: nothing is trusted");
    }

    #[test]
    fn no_owner_configured() {
        let m = parse_message(1, &raw(USER, "Hi", "x", GENUINE_AR, ""), "");
        assert_eq!(verified_owner(&m, None, G), None);
        assert_eq!(verified_owner(&m, Some("  "), G), None);
    }
}
