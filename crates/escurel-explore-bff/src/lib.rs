//! escurel-explore-bff — an auth-bridging reverse proxy (Backend-For-
//! Frontend) for the `escurel-explore` Flutter SPA.
//!
//! The SPA runs in a browser, which cannot safely hold a signing key, so
//! it cannot present the signed OIDC JWT that escurel's `/mcp` requires.
//! This BFF closes that gap: it serves the SPA bundle and, on each
//! `POST /mcp`, MINTS a fresh short-lived RS256 JWT and forwards the
//! request to escurel with that bearer.
//!
//! The BFF is its own OIDC issuer: it publishes its public key at
//! `/jwks.json`, which escurel is configured to trust (matching
//! [`escurel_auth::OidcVerifier`]'s contract). Tokens are minted to mirror
//! the canonical reference in `escurel-test-support`'s issuer:
//! `alg=RS256`, `kid`, and claims `iss` / `aud` / `sub` / `tenant` /
//! `roles` / `iat` / `exp` (~300s).
//!
//! # HTTP surface
//! - `GET /healthz` → `200 "ok"`. Dependency-free liveness (substrate
//!   contract): NEVER touches the backend.
//! - `GET /version` → reverse-proxy `${backend}/version` (status + body +
//!   content-type pass-through); `502` if the backend is unreachable.
//! - `GET /jwks.json` → the BFF's public JWKS.
//! - `POST /mcp` → mint a JWT, forward the verbatim body to
//!   `${backend}/mcp` with `Authorization: Bearer …`, relay the response.
//! - `GET /*` → serve a file from the bundle dir, falling back to
//!   `index.html` for SPA deep links.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::services::{ServeDir, ServeFile};

/// Runtime configuration. All fields have sensible local defaults via
/// [`Config::from_env`]; tests construct this struct directly.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory holding the built Flutter web bundle to serve at `/`.
    pub bundle_dir: String,
    /// escurel base URL to proxy `/mcp` and `/version` to.
    pub backend: String,
    /// The `iss` the BFF stamps — must equal the issuer escurel trusts.
    pub issuer: String,
    /// The `aud` the BFF stamps — must equal escurel's audience.
    pub audience: String,
    /// Tenant id routed via the `tenant` claim.
    pub tenant: String,
    /// JWT `kid` — also the `kid` published in the JWKS.
    pub kid: String,
    /// RSA private key, PKCS#8 PEM. `None` → generate an ephemeral
    /// 2048-bit keypair at boot.
    pub signing_key_pem: Option<String>,
    /// Group/role names to grant the explorer for escurel's group ACL
    /// (RBAC v1), emitted under [`Self::groups_claim`]. Empty (default) →
    /// no groups claim, so the explorer is `public`/`owner`-only +
    /// admin-bypass exactly as before. Set to grant Carl the groups its
    /// editable skills require.
    pub groups: Vec<String>,
    /// The claim name the groups are emitted under — must equal escurel's
    /// configured `groups_claim`. Default `triton_sender_groups` (the
    /// substrate-unified groups claim, kept distinct from `roles` so the
    /// `roles`/admin-derivation path is never conflated with data groups).
    pub groups_claim: String,
}

impl Config {
    /// Read the `EXPLORE_BFF_*` env surface, falling back to local
    /// defaults. The `listen` address is returned separately by
    /// [`listen_from_env`] because it isn't part of the router state.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            bundle_dir: env_or("EXPLORE_BFF_BUNDLE_DIR", "/usr/share/escurel-explore/web"),
            backend: env_or("EXPLORE_BFF_ESCUREL_BACKEND", "http://localhost:8081"),
            issuer: env_or("EXPLORE_BFF_ISSUER", "http://localhost:8080"),
            audience: env_or("EXPLORE_BFF_AUDIENCE", "escurel-nonprod"),
            tenant: env_or("EXPLORE_BFF_TENANT", "default"),
            kid: env_or("EXPLORE_BFF_KID", "escurel-explore"),
            signing_key_pem: std::env::var("EXPLORE_BFF_SIGNING_KEY").ok(),
            groups: env_or("EXPLORE_BFF_GROUPS", "")
                .split([',', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            groups_claim: env_or("EXPLORE_BFF_GROUPS_CLAIM", "triton_sender_groups"),
        }
    }
}

/// The listen address from `EXPLORE_BFF_LISTEN` (default `0.0.0.0:8080`).
#[must_use]
pub fn listen_from_env() -> String {
    env_or("EXPLORE_BFF_LISTEN", "0.0.0.0:8080")
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

/// The RSA signing material the BFF uses, plus the JWKS-facing modulus +
/// exponent. Built once at boot; cheap to share across requests.
struct Signer {
    /// PKCS#1 PEM the `jsonwebtoken` RS256 path accepts.
    private_pem: Vec<u8>,
    kid: String,
    /// base64url RSA modulus for the published JWKS.
    n_b64: String,
    /// base64url RSA exponent for the published JWKS.
    e_b64: String,
}

impl Signer {
    /// Build from an optional PKCS#8 PEM private key; generate an
    /// ephemeral 2048-bit keypair when `None`. Returns whether a
    /// configured key (vs an ephemeral one) was used, for logging.
    fn build(signing_key_pem: Option<&str>, kid: &str) -> Result<(Self, bool), BootError> {
        let (private, configured) = match signing_key_pem {
            Some(raw) => (
                RsaPrivateKey::from_pkcs8_pem(&normalize_pem(raw)).map_err(BootError::KeyParse)?,
                true,
            ),
            None => {
                let mut rng = rand::thread_rng();
                (
                    RsaPrivateKey::new(&mut rng, 2048).map_err(BootError::KeyGen)?,
                    false,
                )
            }
        };
        let public = RsaPublicKey::from(&private);
        // `jsonwebtoken::EncodingKey::from_rsa_pem` accepts PKCS#1; re-
        // encode regardless of the input encoding so a PKCS#8 input also
        // works (mirrors escurel-test-support's `Keys`).
        let private_pem = private
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .map_err(BootError::KeyEncode)?
            .as_bytes()
            .to_vec();
        let signer = Self {
            private_pem,
            kid: kid.to_owned(),
            n_b64: b64url(&public.n().to_bytes_be()),
            e_b64: b64url(&public.e().to_bytes_be()),
        };
        Ok((signer, configured))
    }

    /// The published JWKS document.
    fn jwks(&self) -> serde_json::Value {
        json!({
            "keys": [{
                "kid": self.kid,
                "kty": "RSA",
                "alg": "RS256",
                "use": "sig",
                "n": self.n_b64,
                "e": self.e_b64,
            }]
        })
    }

    /// Mint a fresh short-lived RS256 bearer per the escurel contract.
    fn mint(&self, cfg: &Config) -> Result<String, jsonwebtoken::errors::Error> {
        let now = now_secs();
        let mut claims = serde_json::Map::new();
        claims.insert("iss".into(), json!(cfg.issuer));
        claims.insert("aud".into(), json!(cfg.audience));
        claims.insert("sub".into(), json!("escurel-explore"));
        // The tenant claim name escurel verifies against (default `tenant`).
        claims.insert(TENANT_CLAIM.into(), json!(cfg.tenant));
        // Least privilege: a non-admin role. The verifier's default
        // `admin_role_value` is `escurel:admin`; this deliberately differs
        // so the token projects to `Role::Agent`.
        claims.insert("roles".into(), json!(["escurel:agent"]));
        // Group ACL v1: grant the explorer the configured groups under the
        // claim escurel reads (`groups_claim`). Kept off `roles` so it never
        // touches admin derivation. Omitted when no groups are configured —
        // the explorer then resolves to `public`/`owner` only (unchanged).
        if !cfg.groups.is_empty() {
            claims.insert(cfg.groups_claim.clone(), json!(cfg.groups));
        }
        claims.insert("iat".into(), json!(now));
        claims.insert("exp".into(), json!(now + 300));
        let claims = serde_json::Value::Object(claims);
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        let key = EncodingKey::from_rsa_pem(&self.private_pem)?;
        encode(&header, &claims, &key)
    }
}

/// The tenant claim name escurel's verifier reads by default.
const TENANT_CLAIM: &str = "tenant";

/// Shared router state.
struct AppState {
    cfg: Config,
    signer: Signer,
    http: reqwest::Client,
}

/// Boot-time failure building the signer.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error("signing key PEM could not be parsed as PKCS#8: {0}")]
    KeyParse(rsa::pkcs8::Error),
    #[error("ephemeral RSA key generation failed: {0}")]
    KeyGen(rsa::Error),
    #[error("re-encoding the RSA key failed: {0}")]
    KeyEncode(rsa::pkcs1::Error),
}

/// Build the BFF router from config. Generates the ephemeral key (if no
/// `signing_key_pem` is set) here, so a misconfigured key fails fast.
///
/// # Panics
/// Panics if the signing material cannot be built. Use [`try_app`] to
/// handle the error; `app` is the ergonomic entry point for `main` and
/// tests where a bad key is a fatal misconfiguration.
pub fn app(cfg: Config) -> Router {
    try_app(cfg).expect("BFF signer built")
}

/// Fallible router builder. See [`app`].
pub fn try_app(cfg: Config) -> Result<Router, BootError> {
    let (signer, configured) = Signer::build(cfg.signing_key_pem.as_deref(), &cfg.kid)?;
    if configured {
        tracing::info!(kid = %cfg.kid, "using configured RSA signing key");
    } else {
        tracing::info!(kid = %cfg.kid, "no EXPLORE_BFF_SIGNING_KEY set — generated ephemeral 2048-bit RSA keypair");
    }

    let state = Arc::new(AppState {
        http: reqwest::Client::new(),
        signer,
        cfg,
    });

    // SPA fallback: any unmatched GET resolves a file from the bundle
    // dir, falling back to index.html for deep links. Missing dir /
    // index.html is fine (404) — no panic at boot.
    let index = format!("{}/index.html", state.cfg.bundle_dir);
    let serve_dir = ServeDir::new(&state.cfg.bundle_dir).fallback(ServeFile::new(index));

    Ok(Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
        .route("/jwks.json", get(jwks))
        .route("/mcp", post(mcp))
        .fallback_service(serve_dir)
        .with_state(state))
}

/// `GET /healthz` — dependency-free liveness. Always 200 once bound.
async fn healthz() -> &'static str {
    "ok"
}

/// `GET /jwks.json` — the BFF's public signing key as a JWKS.
async fn jwks(State(state): State<Arc<AppState>>) -> Response {
    axum::Json(state.signer.jwks()).into_response()
}

/// `GET /version` — reverse-proxy to `${backend}/version`, passing the
/// upstream status, body, and content-type through. `502` if the backend
/// is unreachable.
async fn version(State(state): State<Arc<AppState>>) -> Response {
    let url = format!("{}/version", state.cfg.backend.trim_end_matches('/'));
    match state.http.get(&url).send().await {
        Ok(resp) => relay(resp).await,
        Err(e) => {
            tracing::warn!(error = %e, "backend /version unreachable");
            (StatusCode::BAD_GATEWAY, "backend unreachable").into_response()
        }
    }
}

/// `POST /mcp` — mint a fresh JWT and forward the verbatim body to
/// `${backend}/mcp`, relaying escurel's response.
async fn mcp(State(state): State<Arc<AppState>>, _headers: HeaderMap, body: Bytes) -> Response {
    let token = match state.signer.mint(&state.cfg) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "failed to mint JWT");
            return (StatusCode::INTERNAL_SERVER_ERROR, "token mint failed").into_response();
        }
    };
    let url = format!("{}/mcp", state.cfg.backend.trim_end_matches('/'));
    let sent = state
        .http
        .post(&url)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await;
    match sent {
        Ok(resp) => relay(resp).await,
        Err(e) => {
            tracing::warn!(error = %e, "backend /mcp unreachable");
            (StatusCode::BAD_GATEWAY, "backend unreachable").into_response()
        }
    }
}

/// Relay an upstream `reqwest::Response` verbatim: status + content-type +
/// body.
async fn relay(resp: reqwest::Response) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "reading upstream body failed");
            return (StatusCode::BAD_GATEWAY, "upstream body error").into_response();
        }
    };
    let mut out = Response::builder().status(status);
    if let Some(ct) = content_type {
        out = out.header(header::CONTENT_TYPE, ct);
    }
    out.body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Serve a built router on an already-bound listener until shutdown.
/// Tests bind first (to learn the port) and hand the listener in.
///
/// # Errors
/// Propagates any `axum::serve` I/O error.
pub async fn serve_on(listener: TcpListener, app: Router) -> std::io::Result<()> {
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
}

/// Bind `listen` and serve `app`. Convenience wrapper over [`serve_on`].
///
/// # Errors
/// Propagates bind / serve I/O errors.
pub async fn serve(listen: &str, app: Router) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(addr = %listener.local_addr()?, "escurel-explore-bff listening");
    serve_on(listener, app).await
}

/// Resolve when the process is asked to stop. The substrate stops
/// containers with `SIGTERM` (12-factor / substrate contract), so we wait
/// on both `SIGTERM` and `SIGINT` (Ctrl-C) and shut down gracefully on
/// either.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Accept the signing key as either a raw PKCS#8 PEM or a base64-encoded
/// PEM. The substrate seeds signing keys into Secret Manager base64-encoded
/// (the `dz-carl` convention — PEM newlines are awkward in env vars); a raw
/// PEM (e.g. a local `EXPLORE_BFF_SIGNING_KEY=$(cat key.pem)`) also works.
fn normalize_pem(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains("-----BEGIN") {
        return trimmed.to_owned();
    }
    // Not already PEM → assume base64-wrapped PEM. Strip whitespace the
    // env var may carry, then decode; fall back to the raw string so the
    // parse error surfaces against the original input.
    let compact: String = trimmed.split_whitespace().collect();
    match base64::engine::general_purpose::STANDARD.decode(compact.as_bytes()) {
        Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| trimmed.to_owned()),
        Err(_) => trimmed.to_owned(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};

    /// A configured key accepted as raw PKCS#8 PEM *and* as base64-wrapped
    /// PEM (the substrate's Secret-Manager seeding shape) both build a
    /// signer with the same public modulus.
    #[test]
    fn signing_key_accepts_raw_and_base64_pem() {
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
        let b64 = base64::engine::general_purpose::STANDARD.encode(pem.as_bytes());

        let (raw_signer, configured) = Signer::build(Some(&pem), "k").unwrap();
        assert!(configured, "explicit key → configured, not ephemeral");
        let (b64_signer, _) = Signer::build(Some(&b64), "k").unwrap();
        assert_eq!(
            raw_signer.n_b64, b64_signer.n_b64,
            "raw PEM and base64 PEM resolve to the same key"
        );
    }
}
