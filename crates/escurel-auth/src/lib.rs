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
mod verifier;

pub use jwks::{Jwks, JwksCache};
pub use verifier::{AuthContext, AuthError, OidcConfig, OidcVerifier, Role};
