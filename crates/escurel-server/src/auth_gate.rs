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
