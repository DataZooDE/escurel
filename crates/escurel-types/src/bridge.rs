//! A chat-channel bridge acting for a person (#824).
//!
//! The bridge (`escurel-chat-bridge`) signs short-lived tokens for the people
//! it answers, under a key of its own and an issuer the gateway names in
//! `ESCUREL_AUTH_BRIDGE_ISSUER` (pinned to the key's `kid`,
//! `ESCUREL_AUTH_BRIDGE_KID`). The gateway's verifier restricts every token
//! from that issuer, whatever kid it names:
//! a short TTL, no roles or groups, no agent or run claims, and an actor of
//! `bridge:<channel>` — which the gateway stamps into what the token captures
//! as `captured_via`, so a turn the bridge relayed is told apart from one the
//! person typed themselves. The runner's chat rule and the gateway's page-chat
//! reply rule accept exactly this via and no other.

/// The prefix of a bridge's `captured_via`: `bridge:<channel>`.
pub const VIA_PREFIX: &str = "bridge:";

/// The claim a bridge token names its channel in (`msteams`, …).
pub const CHANNEL_CLAIM: &str = "bridge_channel";

/// The `label_skill` a bridge captures its chat turns under unless configured
/// otherwise (bridge `ESCUREL_BRIDGE_CHAT_SKILL`, gateway
/// `ESCUREL_AUTH_BRIDGE_CHAT_SKILL`): the only label a bridge token may use.
pub const DEFAULT_CHAT_SKILL: &str = "chat";

/// The longest a bridge token may live (`exp - iat`, and `exp - now`).
pub const MAX_TTL_SECS: u64 = 900;

/// A channel name a bridge may claim: 1–32 of `[a-z0-9-]`, starting with a
/// letter. Narrow on purpose: it becomes part of `captured_via`.
#[must_use]
pub fn is_valid_channel(channel: &str) -> bool {
    let mut chars = channel.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && channel.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `bridge:<channel>` for a valid channel name.
#[must_use]
pub fn via(channel: &str) -> Option<String> {
    is_valid_channel(channel).then(|| format!("{VIA_PREFIX}{channel}"))
}

/// Whether a server-stamped `captured_via` names a bridge.
#[must_use]
pub fn is_bridge_via(captured_via: &str) -> bool {
    captured_via
        .strip_prefix(VIA_PREFIX)
        .is_some_and(is_valid_channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_channel_name_makes_a_via() {
        assert_eq!(via("msteams").as_deref(), Some("bridge:msteams"));
        for bad in ["", "MSTeams", "ms teams", "a:b", "-x", &"x".repeat(33)] {
            assert_eq!(via(bad), None, "{bad}");
        }
        assert!(is_bridge_via("bridge:msteams"));
        assert!(!is_bridge_via("escurel-runner"));
        assert!(!is_bridge_via("bridge:"));
        assert!(!is_bridge_via("bridge:Evil Runner"));
    }
}
