//! OIDC token verification + tenant resolution for Escurel.
//!
//! `OidcVerifier::verify(token)` takes a bearer JWT, validates it
//! against the issuer's JWKS (fetched + cached), and projects it
//! into an [`AuthContext`] that downstream layers (gateway, quota,
//! Indexer dispatch) can route on.
//!
//! Configuration follows `docs/spec/platform.md §Auth`:
//!
//! ```toml
//! [auth]
//! oidc_issuer        = "https://auth.example.com/realms/main"
//! oidc_audience      = "escurel"
//! tenant_claim       = "tenant"        # which JWT claim names the tenant
//! admin_role_claim   = "roles"         # which claim lists role memberships
//! admin_role_value   = "escurel:admin" # role value that grants admin access
//! jwks_refresh_secs  = 300
//! ```

mod jwks;
mod signer;
mod verifier;

pub use jwks::{Jwks, JwksCache};
pub use signer::{
    DELEGATION_PURPOSE, PURPOSE_CLAIM, ROOT_EVENT_ID_CLAIM, RUN_ID_CLAIM,
    RunClaims as MintRunClaims, SignError, Signer, TENANT_CLAIM, TRACE_ID_CLAIM,
    WORKBENCH_AGENT_PURPOSE,
};
pub use verifier::{AuthContext, AuthError, OidcConfig, OidcVerifier, Role, RunClaims};
