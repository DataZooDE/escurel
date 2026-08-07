//! Shared HTTP auth gate for the tenant-scoped routes.
//!
//! `/mcp` + `/ingest` (see [`crate::mcp`]) and `/ws` (see [`crate::ws`])
//! authenticate identically: a `Bearer` JWT verified by the
//! [`OidcVerifier`], then the hard one-instance-one-tenant boundary. This
//! module is the single definition so the HTTP and WebSocket gates cannot
//! drift apart.

use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use escurel_auth::{AuthContext, OidcVerifier};
use serde_json::json;

/// Authenticate a request against `verifier` and enforce the tenant
/// boundary. Returns the verified [`AuthContext`], or a ready-to-return
/// error response: `401` for a missing/invalid token, `403` for a
/// validly-signed token whose `tenant` claim is not `served_tenant`.
///
/// Hard tenant boundary: one instance serves exactly one tenant. A token
/// minted for a different tenant (same issuer/audience) must be refused —
/// never silently operate on the served tenant's corpus. Enforced for every
/// role, including admin (an operator uses a tenant-scoped token per
/// instance) and including the admin tenant-CRUD tools that dispatch ahead
/// of the indexer gate — the served tenant comes from config, not the
/// indexer, so it holds even for a control-plane deployment with no indexer.
/// Skipped only when no served tenant is configured (an unconfigured dev
/// gateway, which also runs without a verifier).
pub(crate) async fn enforce_auth(
    verifier: &OidcVerifier,
    headers: &HeaderMap,
    served_tenant: Option<&str>,
) -> Result<AuthContext, axum::response::Response> {
    let Some(token) = bearer_token(headers) else {
        return Err(auth_failure("missing Authorization: Bearer header"));
    };
    let ctx = verifier
        .verify(&token)
        .await
        .map_err(|e| auth_failure(format!("token rejected: {e}")))?;
    if let Some(served) = served_tenant
        && ctx.tenant_id != served
    {
        return Err(forbidden_tenant(&ctx.tenant_id, served));
    }
    Ok(ctx)
}

/// `403` for a validly-signed token whose tenant claim is not the one this
/// instance serves. Distinct from [`auth_failure`] (`401`, a bad/absent token).
fn forbidden_tenant(token_tenant: &str, served: &str) -> axum::response::Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "forbidden",
            "message": format!(
                "token tenant `{token_tenant}` is not served by this instance (serves `{served}`)"
            ),
        })),
    )
        .into_response()
}

/// Extract the bearer token from the `Authorization` header (case-insensitive
/// scheme), or `None` when the header is absent / malformed.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("authorization")?.to_str().ok()?;
    if let Some(stripped) = raw.strip_prefix("Bearer ") {
        return Some(stripped.trim().to_owned());
    }
    if let Some(stripped) = raw.strip_prefix("bearer ") {
        return Some(stripped.trim().to_owned());
    }
    None
}

/// `401` for a missing or invalid token.
fn auth_failure(message: impl Into<String>) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "unauthorized",
            "message": message.into(),
        })),
    )
        .into_response()
}

/// Run the auth gate when a verifier is configured.
///
/// `Ok(None)` means no verifier is wired — dev / on-host mode, where the
/// gateway is open. `Err` is a ready-to-return error response.
///
/// `/mcp` and `/ingest` each open with this exact block. They diverge
/// immediately afterwards (JSON-RPC error envelopes vs plain HTTP JSON, and
/// different quota dimensions), which is why only the block itself is shared
/// — folding the divergent halves together would mean inventing a common
/// error shape neither route wants. R5 of
/// `docs/notes/complexity-reduction-plan.md`.
pub(crate) async fn authenticate(
    state: &crate::server::AppState,
    headers: &HeaderMap,
) -> Result<Option<AuthContext>, axum::response::Response> {
    match state.verifier.as_ref() {
        Some(verifier) => {
            let served = state.served_tenant.as_deref();
            enforce_auth(verifier, headers, served).await.map(Some)
        }
        None => Ok(None),
    }
}

/// RBAC group names from a verified token, with the configured admin role
/// value removed.
///
/// The stripping is a security boundary, not tidying: `admin_role_value`
/// (e.g. `escurel:admin`) arrives in the same `groups` claim as ordinary
/// group names, so leaving it in would let a token grant itself admin
/// authority through a group ACL. Admin authority comes only from the
/// verified [`Role`](escurel_auth::Role) — never from a group name.
/// `escurel-index` strips reserved names (public/owner/admin) again as
/// defence in depth.
///
/// `/mcp` and `/ingest` computed this identically, the second with a comment
/// reading "mirroring `mcp_inner`". A security check maintained by mirroring
/// is one edit away from being two different checks.
pub(crate) fn rbac_groups(
    state: &crate::server::AppState,
    auth_ctx: Option<&AuthContext>,
) -> Vec<String> {
    let admin_value = state
        .verifier
        .as_ref()
        .map(|v| v.config().admin_role_value.clone());
    strip_admin_value(
        auth_ctx.map(|c| c.groups.as_slice()).unwrap_or(&[]),
        admin_value.as_deref(),
    )
}

/// The stripping itself, separated so the security boundary is testable
/// without standing up an `AppState` and a verifier.
fn strip_admin_value(groups: &[String], admin_value: Option<&str>) -> Vec<String> {
    groups
        .iter()
        .filter(|g| Some(g.as_str()) != admin_value)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::strip_admin_value;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_admin_role_value_never_survives_as_a_group() {
        // The whole point: `escurel:admin` arrives in the same claim as
        // ordinary group names, so leaving it in would let a token grant
        // itself admin authority through a group ACL.
        let groups = v(&["eng", "escurel:admin", "ops"]);
        assert_eq!(
            strip_admin_value(&groups, Some("escurel:admin")),
            v(&["eng", "ops"])
        );
    }

    #[test]
    fn ordinary_groups_pass_through_in_order() {
        let groups = v(&["eng", "ops"]);
        assert_eq!(strip_admin_value(&groups, Some("escurel:admin")), groups);
    }

    #[test]
    fn stripping_is_exact_not_prefix_or_substring() {
        // `escurel:admins` is a different group and must survive; a
        // `starts_with`/`contains` implementation would eat it.
        let groups = v(&["escurel:admins", "not-escurel:admin", "escurel:admin"]);
        assert_eq!(
            strip_admin_value(&groups, Some("escurel:admin")),
            v(&["escurel:admins", "not-escurel:admin"])
        );
    }

    #[test]
    fn every_occurrence_is_removed() {
        // A claim carrying the value twice must not leave one behind.
        let groups = v(&["escurel:admin", "eng", "escurel:admin"]);
        assert_eq!(
            strip_admin_value(&groups, Some("escurel:admin")),
            v(&["eng"])
        );
    }

    #[test]
    fn no_configured_admin_value_strips_nothing() {
        let groups = v(&["eng", "escurel:admin"]);
        assert_eq!(strip_admin_value(&groups, None), groups);
    }

    #[test]
    fn an_absent_auth_context_yields_no_groups() {
        assert!(strip_admin_value(&[], Some("escurel:admin")).is_empty());
    }
}
