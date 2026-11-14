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

/// The server-owned provenance key a bridge turn's capture lists the turn's
/// groups under (#830): the groups the GATEWAY resolved for the asker, which
/// the runner's chat run carries. Stamped by the gateway only; a caller's own
/// is removed.
pub const CAPTURED_GROUPS_FIELD: &str = "captured_groups";

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

/// The placeholders a principal template may name.
pub const PRINCIPAL_PLACEHOLDERS: [&str; 3] = ["{aadObjectId}", "{userId}", "{tenantId}"];

/// Refuse a principal template that could map two people to one subject:
/// every `{…}` must be a known placeholder, and at least one of them must
/// name the person (`{aadObjectId}` or `{userId}`) — a constant, a misspelt
/// placeholder (left in as a literal) or a tenant-only template would give
/// everyone the same identity, and with it each other's private pages.
///
/// # Errors
/// Why the template is refused.
pub fn validate_principal_template(template: &str) -> Result<(), String> {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open..];
        let close = after
            .find('}')
            .ok_or_else(|| format!("unclosed `{{` in `{template}`"))?;
        let placeholder = &after[..=close];
        if !PRINCIPAL_PLACEHOLDERS.contains(&placeholder) {
            return Err(format!(
                "unknown placeholder `{placeholder}` (known: {})",
                PRINCIPAL_PLACEHOLDERS.join(", ")
            ));
        }
        rest = &after[close + 1..];
    }
    if rest.contains('}') {
        return Err(format!("stray `}}` in `{template}`"));
    }
    if !(template.contains("{aadObjectId}") || template.contains("{userId}")) {
        return Err(
            "must name the person (`{aadObjectId}` or `{userId}`): otherwise every sender \
             maps to one subject"
                .to_owned(),
        );
    }
    if template.starts_with(crate::chat_channel::GROUP_SUBJECT_PREFIX) {
        return Err(format!(
            "must not start with `{}`: that prefix names a group principal",
            crate::chat_channel::GROUP_SUBJECT_PREFIX
        ));
    }
    Ok(())
}

/// The escurel subject for a platform user: `template` with `{aadObjectId}`,
/// `{userId}` and `{tenantId}` filled in. `None` when the template names a
/// field the user lacks (or has empty) — never a guessed identity.
#[must_use]
pub fn principal_for(
    template: &str,
    aad_object_id: Option<&str>,
    user_id: Option<&str>,
    tenant_id: Option<&str>,
) -> Option<String> {
    let mut out = template.to_owned();
    for (field, value) in [
        ("{aadObjectId}", aad_object_id),
        ("{userId}", user_id),
        ("{tenantId}", tenant_id),
    ] {
        if out.contains(field) {
            out = out.replace(field, value.filter(|v| !v.is_empty())?);
        }
    }
    (!out.is_empty()).then_some(out)
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

    #[test]
    fn a_template_that_maps_everyone_to_one_subject_is_refused() {
        for (template, why) in [
            ("alice", "must name the person"),
            ("user:{aadObjectID}", "unknown placeholder `{aadObjectID}`"),
            ("{tenantId}", "must name the person"),
            ("team:{tenantId}:all", "must name the person"),
            ("user:{aadObjectId", "unclosed"),
            ("user:aadObjectId}", "stray"),
            ("group:{aadObjectId}", "group principal"),
        ] {
            let err = validate_principal_template(template).unwrap_err();
            assert!(err.contains(why), "{template}: {err}");
        }
        for ok in [
            "user:{aadObjectId}",
            "{aadObjectId}",
            "{tenantId}/{userId}",
            "teams:{tenantId}:{aadObjectId}",
        ] {
            validate_principal_template(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
    }

    #[test]
    fn the_principal_comes_from_the_template_or_not_at_all() {
        assert_eq!(
            principal_for("user:{aadObjectId}", Some("oid"), Some("29:x"), Some("t")).as_deref(),
            Some("user:oid")
        );
        assert_eq!(
            principal_for("{tenantId}/{userId}", None, Some("29:x"), Some("t")).as_deref(),
            Some("t/29:x")
        );
        assert_eq!(
            principal_for("user:{aadObjectId}", None, Some("29:x"), None),
            None
        );
        assert_eq!(
            principal_for("user:{aadObjectId}", Some(""), None, None),
            None
        );
    }
}
