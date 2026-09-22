//! The runner's own credential for the gateway.
//!
//! `ESCUREL_RUNNER_TOKEN` is a bearer somebody minted by hand and pasted into
//! a secret. It works, and it has one property that makes it wrong for a
//! deployment: it expires silently. A runner whose token has lapsed still
//! answers `/healthz`, still polls, still marks nothing — it just stops being
//! able to read the inbox, which looks exactly like an empty inbox. The same
//! reasoning already removed static bearers from every other workload on this
//! substrate.
//!
//! So the runner can mint instead. The claim shape and the `kid` derivation
//! here are deliberately IDENTICAL to the ones heron and `agent-core` use,
//! because this borrows the existing signing identity rather than becoming a
//! second issuer: the JWKS the platform already publishes is what verifies
//! these tokens, and no gateway configuration changes.
//!
//! `sub` is a SERVICE principal, not a person — the runner acts on events
//! whose author has gone home. That is also why it carries the admin role:
//! the gateway's write ACL is group-matched, a minted token carries no
//! engagement groups, and an agent that cannot write is a runner that
//! silently does nothing. The privilege is real and worth naming: every run
//! for every consultant currently shares it, and the per-run scoped token is
//! the seam that fixes that (`packager.rs`'s `token` field).

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;

/// The claim the gateway's verifier reads for the tenant.
const TENANT_CLAIM: &str = "tenant";

/// The `purpose` claim on an internal-delegation token (fleet #801 Phase 4). A
/// token the runner presents to the AGENT when delegating a domain step — NOT an
/// escurel token. Shared so the escurel gateway verifier can REJECT a token
/// carrying it (a delegation bearer must never be accepted back into escurel's
/// own `/mcp`).
pub const DELEGATION_PURPOSE: &str = "internal_delegation";
/// The claim key under which [`DELEGATION_PURPOSE`] is carried.
pub const PURPOSE_CLAIM: &str = "purpose";

/// How long a minted bearer lives.
///
/// Long enough that a run and its retries never straddle an expiry; short
/// enough that a leaked one is worth little. Re-minted well before it lapses
/// — see [`REFRESH_MARGIN_SECS`].
pub const TTL_SECS: u64 = 30 * 60;

/// Re-mint this long before expiry, so a token is never handed out with less
/// life left than a slow run might need.
const REFRESH_MARGIN_SECS: u64 = 5 * 60;

/// The run-identity claims on a per-run token (workbench backend P1). Kept in
/// lock-step with `escurel_auth::verifier` (which must not depend on this
/// crate); the gateway reads them to stamp a draft's lineage and to authorise
/// `report_progress` — so they ride on the token the runner SIGNS, never on a
/// header the harness could set.
pub const RUN_ID_CLAIM: &str = "run_id";
pub const ROOT_EVENT_ID_CLAIM: &str = "root_event_id";
pub const TRACE_ID_CLAIM: &str = "trace_id";

/// The run a per-run token belongs to. `trace_id` is the lineage's OTel
/// trace (one per cascade lineage) when the runner has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunClaims {
    pub run_id: String,
    pub root_event_id: String,
    pub trace_id: Option<String>,
}

fn stamp_run(claims: &mut serde_json::Value, run: Option<&RunClaims>) {
    if let Some(run) = run {
        claims[RUN_ID_CLAIM] = json!(run.run_id);
        claims[ROOT_EVENT_ID_CLAIM] = json!(run.root_event_id);
        if let Some(trace) = &run.trace_id {
            claims[TRACE_ID_CLAIM] = json!(trace);
        }
    }
}

/// Errors from building or using the signing identity.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The key did not parse as PKCS#8 or PKCS#1 RSA.
    #[error(
        "ESCUREL_RUNNER_AUTH_SIGNING_KEY is not a valid RSA private key (tried PKCS#8, PKCS#1)"
    )]
    KeyParse,
    /// Re-encoding the parsed key failed.
    #[error("re-encoding the RSA key failed: {0}")]
    KeyEncode(rsa::pkcs1::Error),
    /// Signing failed.
    #[error("minting failed: {0}")]
    Mint(#[from] jsonwebtoken::errors::Error),
    /// The run's `label_skill` cannot become an unambiguous token subject.
    #[error(
        "{0:?} is not usable as a per-run agent subject (expected a skill id of          letters, digits, '_', '-')"
    )]
    UnusableAgentSubject(String),
}

/// Where the runner's gateway bearer comes from.
///
/// Two variants and no fallback between them: a deployment that configured a
/// signing key and gets a stale static token instead would be authenticated
/// as something nobody chose.
pub enum TokenSource {
    /// A bearer supplied whole (`ESCUREL_RUNNER_TOKEN`). Returned unchanged,
    /// expiry and all.
    Static(String),
    /// Minted here, and re-minted before it lapses.
    Minted {
        signer: Signer,
        subject: String,
        /// How long each minted bearer lives. Configurable so a test can
        /// outlive one in seconds rather than half an hour — the only way to
        /// prove a long-running loop actually re-mints.
        ttl_secs: u64,
        /// `(token, expires_at_unix)`.
        cached: Mutex<Option<(String, u64)>>,
    },
}

impl std::fmt::Debug for TokenSource {
    /// Hand-written: neither the key nor a live bearer may reach a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(_) => f.write_str("TokenSource::Static(<redacted>)"),
            Self::Minted { subject, .. } => f
                .debug_struct("TokenSource::Minted")
                .field("subject", subject)
                .finish_non_exhaustive(),
        }
    }
}

impl TokenSource {
    /// A usable bearer: the static one, or a minted one with life left.
    ///
    /// # Errors
    /// When minting fails.
    pub fn current(&self) -> Result<String, AuthError> {
        match self {
            Self::Static(t) => Ok(t.clone()),
            Self::Minted {
                signer,
                subject,
                ttl_secs,
                cached,
            } => {
                let ttl = *ttl_secs;
                let now = now_secs();
                let mut slot = cached.lock().unwrap_or_else(|e| e.into_inner());
                // A TTL at or under the margin re-mints every call, which is
                // exactly what a short-lived test TTL should do.
                if let Some((token, exp)) = slot.as_ref()
                    && *exp > now + REFRESH_MARGIN_SECS.min(ttl / 2)
                {
                    return Ok(token.clone());
                }
                let token = signer.mint(subject, ttl)?;
                *slot = Some((token.clone(), now + ttl));
                Ok(token)
            }
        }
    }

    /// Mint a **per-run, caller-scoped** bearer (async-ops Phase 2c-i) for the
    /// verified requester `subject` with their RBAC `groups`. Not cached — each
    /// run scopes its own.
    ///
    /// Returns `None` for a [`Self::Static`] source: it holds a bearer, not a
    /// signing key, so it cannot mint a scoped token. The caller then falls back
    /// to the runner's own identity — the confused deputy persists until the
    /// runner runs in **minted** mode, which production does (ADR-0012: the
    /// runner mints its own bearer from a per-tenant GCP Secret Manager key).
    ///
    /// # Errors
    /// When signing fails.
    pub fn mint_scoped(
        &self,
        subject: &str,
        groups: &[String],
        run: Option<&RunClaims>,
    ) -> Result<Option<String>, AuthError> {
        match self {
            Self::Static(_) => Ok(None),
            Self::Minted {
                signer, ttl_secs, ..
            } => Ok(Some(signer.mint_scoped(subject, groups, *ttl_secs, run)?)),
        }
    }

    /// Mint a **runner→agent delegation** bearer (fleet #801 Phase 4, AD-7) for
    /// a `harness: delegate` step: aud=the agent's audience, empty roles,
    /// `purpose=internal_delegation`, `obo`=the verified requester (audit only).
    /// See [`Signer::mint_delegation`].
    ///
    /// Returns `None` for a [`Self::Static`] source (a bearer, not a signing
    /// key). A static-mode runner therefore cannot delegate — the delegate
    /// harness fails closed rather than presenting the runner's own escurel
    /// bearer to the agent; production runs minted (ADR-0012).
    ///
    /// # Errors
    /// When signing fails.
    pub fn mint_delegation(
        &self,
        audience: &str,
        on_behalf_of: &str,
        step: &str,
    ) -> Result<Option<String>, AuthError> {
        match self {
            Self::Static(_) => Ok(None),
            Self::Minted {
                signer,
                subject,
                ttl_secs,
                ..
            } => Ok(Some(signer.mint_delegation(
                subject,
                audience,
                on_behalf_of,
                step,
                *ttl_secs,
            )?)),
        }
    }

    /// Mint a **per-run agent** bearer (#510) for the run's `label_skill`:
    /// `sub` = `agent:<label_skill>`, with the runner kept as the delegating
    /// actor in `act.sub`. This is the identity the gateway stamps into
    /// `pages.last_written_by` and `crdt_ops.principal`, so two skills leave
    /// two distinguishable audit trails instead of both reading
    /// `escurel-runner`.
    ///
    /// Never cached — each run mints its own, `ttl_secs` bounded by the run
    /// budget. A client built around one of these must therefore live no
    /// longer than its run (the frozen-bearer defect #442).
    ///
    /// Returns `None` for a [`Self::Static`] source: it holds a bearer, not a
    /// signing key, so it cannot scope anything and the caller falls back to
    /// the runner's own identity (dev-only; production runs minted, ADR-0012).
    ///
    /// # Errors
    /// When signing fails, or `label_skill` cannot be an unambiguous subject.
    pub fn mint_agent(
        &self,
        label_skill: &str,
        ttl_secs: u64,
        run: Option<&RunClaims>,
    ) -> Result<Option<String>, AuthError> {
        match self {
            Self::Static(_) => Ok(None),
            Self::Minted {
                signer, subject, ..
            } => Ok(Some(signer.mint_agent(
                subject,
                label_skill,
                ttl_secs,
                run,
            )?)),
        }
    }

    /// Whether this source can mint per-run, caller-scoped tokens — true only in
    /// **minted** mode (a signing key), false for a [`Self::Static`] bearer.
    ///
    /// The fail-closed on a missing requester (crew final-review F2) is gated on
    /// this: only a minting runner actually scopes each run to its requester, so
    /// only there does a run board with no requester signal the strip-mid-run
    /// escalation to refuse. A static-bearer runner cannot scope any run — the
    /// confused deputy is a documented dev-only limitation until minted mode
    /// (ADR-0012) — so a missing requester there is not a new hole to fail on.
    #[must_use]
    pub fn can_mint(&self) -> bool {
        matches!(self, Self::Minted { .. })
    }

    /// The subject this source authenticates AS — the identity the gateway
    /// stamps as `provenance.captured_by` on events this runner captures. The
    /// runner uses it to recognise its OWN emitted events (whose lineage it may
    /// trust for loop control) versus a caller's (whose forged lineage it must
    /// not — the runner-lineage-forge guard). For a minted source it is the
    /// configured subject; for a static bearer it is the JWT `sub`, read
    /// WITHOUT verification — we are only recognising our own token, not
    /// trusting a third party. `None` when a static token has no readable `sub`.
    #[must_use]
    pub fn subject(&self) -> Option<String> {
        match self {
            Self::Minted { subject, .. } => Some(subject.clone()),
            Self::Static(token) => jwt_sub(token),
        }
    }
}

/// Read the `sub` claim from a JWT WITHOUT verifying the signature. Used only to
/// learn this runner's OWN subject from its configured static bearer — never to
/// authenticate a third party.
fn jwt_sub(token: &str) -> Option<String> {
    let payload_b64 = token.split('.').nth(1)?;
    // JWT segments are base64url without padding; be tolerant of either.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("sub")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The RSA signing identity, built once at boot.
pub struct Signer {
    issuer: String,
    audience: String,
    tenant: String,
    kid: String,
    /// PKCS#1 PEM — the flavour `jsonwebtoken`'s RS256 path accepts.
    private_pem: Vec<u8>,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("tenant", &self.tenant)
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl Signer {
    /// Build from an issuer, audience, tenant, optional `kid` and the key.
    ///
    /// Both PEM flavours are accepted deliberately: Secret Manager holds the
    /// shared key as PKCS#8, while most key-generation paths emit PKCS#1.
    /// Rejecting one is a boot failure whose message points nowhere near the
    /// cause.
    ///
    /// # Errors
    /// When the key does not parse or cannot be re-encoded.
    pub fn build(
        issuer: String,
        audience: String,
        tenant: String,
        kid: Option<String>,
        signing_key_pem: &str,
    ) -> Result<Self, AuthError> {
        let raw = signing_key_pem.trim();
        let private = RsaPrivateKey::from_pkcs8_pem(raw)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(raw))
            .map_err(|_| AuthError::KeyParse)?;
        let private_pem = private
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .map_err(AuthError::KeyEncode)?
            .as_bytes()
            .to_vec();

        let public = RsaPublicKey::from(&private);
        let n_b64 = b64url(&public.n().to_bytes_be());
        let e_b64 = b64url(&public.e().to_bytes_be());
        let kid = kid.unwrap_or_else(|| derive_kid(&n_b64, &e_b64));

        Ok(Self {
            issuer,
            audience,
            tenant,
            kid,
            private_pem,
        })
    }

    /// Mint a service bearer for `subject`, valid `ttl_secs`.
    ///
    /// # Errors
    /// When signing fails.
    pub fn mint(&self, subject: &str, ttl_secs: u64) -> Result<String, AuthError> {
        // The runner's own orchestration identity: admin. Its writes include
        // the reserved `escurel:run-status` status events, which the gateway
        // admits only for admin (async-ops F1). This is NOT the identity a
        // background RUN executes under — that is [`Self::mint_scoped`].
        self.mint_with_roles(subject, &["escurel:admin".to_owned()], ttl_secs, None)
    }

    /// Mint a **per-run, caller-scoped** bearer (async-ops Phase 2c-i): the
    /// verified requester's `subject` and their RBAC `groups` as the `roles`
    /// claim — deliberately NOT `escurel:admin`. A background run's harness
    /// executes under this token, so its `/mcp` reads and writes are ACL'd to
    /// exactly what the requester may do, closing the confused deputy (a run
    /// previously executed with the runner's admin identity).
    ///
    /// **Privilege ceiling (crew final-review F1).** Any `escurel:`-prefixed
    /// role in `groups` is STRIPPED before signing — most critically
    /// `escurel:admin`, the value the gateway reads for admin. The requester's
    /// groups reach this from the run board's *mutable* frontmatter, so without
    /// this a requester who overwrote their board with
    /// `requester_groups: ["escurel:admin"]` would get the harness running as
    /// tenant admin. A per-run token can only ever carry a caller's own
    /// engagement groups, never a reserved/privileged role.
    ///
    /// # Errors
    /// When signing fails.
    pub fn mint_scoped(
        &self,
        subject: &str,
        groups: &[String],
        ttl_secs: u64,
        run: Option<&RunClaims>,
    ) -> Result<String, AuthError> {
        let scoped: Vec<String> = groups
            .iter()
            .filter(|g| !g.starts_with("escurel:"))
            .cloned()
            .collect();
        self.mint_with_roles(subject, &scoped, ttl_secs, run)
    }

    /// Mint an INTERNAL-DELEGATION bearer (fleet #801 Phase 4, AD-7): the token
    /// the runner presents to the AGENT's A2A endpoint when it delegates a domain
    /// step. It reuses the runner's key/kid/JWKS — so the agent verifies it
    /// against the identity's published JWKS, needing no new issuer — but it is
    /// deliberately NOT an escurel token:
    ///
    /// - `aud` is the AGENT's audience (a parameter), never escurel's own, so an
    ///   escurel token and a delegation token are never interchangeable.
    /// - `roles: []` — a delegation carries NO escurel authority. The agent
    ///   authorizes the runner service principal and resolves the requester's
    ///   entitlements itself; escurel does not read this token at all.
    /// - `purpose = internal_delegation` — the escurel gateway verifier REJECTS
    ///   this purpose, so a leaked/mis-routed delegation token can never be
    ///   replayed back into escurel's own `/mcp`.
    /// - `obo` carries the verified requester for AUDIT only (never re-read for
    ///   an authorization decision); `step` is the delegated step's id; a random
    ///   `jti` + `nbf` bound replay and validity. TTL should be ≤ the step timeout.
    ///
    /// # Errors
    /// When signing fails.
    pub fn mint_delegation(
        &self,
        subject: &str,
        audience: &str,
        on_behalf_of: &str,
        step: &str,
        ttl_secs: u64,
    ) -> Result<String, AuthError> {
        let now = now_secs();
        let claims = json!({
            "iss": self.issuer,
            "aud": audience,
            "sub": subject,
            TENANT_CLAIM: self.tenant,
            "roles": Vec::<String>::new(),
            PURPOSE_CLAIM: DELEGATION_PURPOSE,
            "obo": on_behalf_of,
            "step": step,
            "jti": ulid::Ulid::new().to_string().to_ascii_lowercase(),
            "iat": now,
            "nbf": now,
            "exp": now + ttl_secs,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        Ok(encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(&self.private_pem)?,
        )?)
    }

    /// Mint a **per-run agent** bearer (#510): the run executes as
    /// `agent:<label_skill>` rather than as the runner, while `act.sub` keeps
    /// the runner visible as the actor that delegated to it (the RFC 8693
    /// delegation shape — "runner acting as inbox-scan").
    ///
    /// The authority is deliberately unchanged from [`Self::mint`] — same
    /// tenant, audience and `escurel:admin` role — because this lands the
    /// plumbing, not a narrowing. Narrowing the grant per skill is the
    /// follow-up that becomes possible once the token is per-run at all
    /// (#510 proposal step 2).
    ///
    /// `ttl_secs` should be the run budget: a run's token has no business
    /// outliving the run that carries it.
    ///
    /// # Errors
    /// When signing fails, or `label_skill` cannot be an unambiguous subject.
    pub fn mint_agent(
        &self,
        runner_subject: &str,
        label_skill: &str,
        ttl_secs: u64,
        run: Option<&RunClaims>,
    ) -> Result<String, AuthError> {
        let slug = agent_slug(label_skill)?;
        let now = now_secs();
        let mut claims = json!({
            "iss": self.issuer,
            "aud": self.audience,
            "sub": format!("agent:{slug}"),
            TENANT_CLAIM: self.tenant,
            // Same authority the runner holds today — see the doc comment.
            "roles": ["escurel:admin"],
            // RFC 8693 §4.1: who is acting, i.e. the delegation chain.
            "act": { "sub": runner_subject },
            // Distinguishes two runs of the SAME skill, and makes a leaked
            // per-run token traceable to the run that leaked it.
            "jti": ulid::Ulid::new().to_string().to_ascii_lowercase(),
            "iat": now,
            "nbf": now,
            "exp": now + ttl_secs,
        });
        stamp_run(&mut claims, run);
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        Ok(encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(&self.private_pem)?,
        )?)
    }

    fn mint_with_roles(
        &self,
        subject: &str,
        roles: &[String],
        ttl_secs: u64,
        run: Option<&RunClaims>,
    ) -> Result<String, AuthError> {
        let now = now_secs();
        let mut claims = json!({
            "iss": self.issuer,
            "aud": self.audience,
            "sub": subject,
            TENANT_CLAIM: self.tenant,
            "roles": roles,
            "iat": now,
            "exp": now + ttl_secs,
        });
        stamp_run(&mut claims, run);
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        Ok(encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(&self.private_pem)?,
        )?)
    }
}

/// A `label_skill` usable as the `agent:` half of a token subject.
///
/// Rejected rather than normalised: a skill id that needed cleaning up could
/// be cleaned into another agent's identity, and an audit trail whose subjects
/// collide is worse than one that admits it cannot name the actor. Accepts what
/// a skill page id actually is — letters, digits, `_`, `-`, `.` — and nothing
/// that could make the subject ambiguous (whitespace, `:` the claim separator,
/// path syntax).
fn agent_slug(label_skill: &str) -> Result<&str, AuthError> {
    let ok = !label_skill.is_empty()
        && !label_skill.contains("..")
        && label_skill
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if ok {
        Ok(label_skill)
    } else {
        Err(AuthError::UnusableAgentSubject(label_skill.to_owned()))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `kid`, in the platform's scheme.
///
/// The `agent-escurel-` prefix is NOT decoration. This borrows the platform's
/// Escurel identity by sharing its key, so the gateway verifies against the
/// JWKS that identity publishes — and that document names the key
/// `agent-escurel-<fingerprint>`. A bare fingerprint is a `kid` the gateway
/// has never seen, and every token is then rejected on a `kid` miss rather
/// than on anything to do with the signature.
///
/// Derived rather than pinned, so rotating the key rotates the identifier.
fn derive_kid(n_b64: &str, e_b64: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(n_b64.as_bytes());
    h.update(b".");
    h.update(e_b64.as_bytes());
    format!("agent-escurel-{}", &b64url(&h.finalize())[..12])
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real 2048-bit RSA key, generated for this test.
    fn test_key() -> String {
        use rsa::pkcs8::EncodePrivateKey;
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("generate");
        key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("encode")
            .to_string()
    }

    #[test]
    fn a_minted_token_carries_the_claims_the_gateway_verifies() {
        let signer = Signer::build(
            "https://agent-lab.data-zoo.de".into(),
            "escurel".into(),
            "datazoo-loops".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let token = signer.mint("escurel-runner", 600).expect("mint");

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWT has three parts: {token}");
        let decode = |s: &str| {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .expect("b64");
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
        };
        let header = decode(parts[0]);
        let claims = decode(parts[1]);

        assert_eq!(header["alg"], "RS256");
        assert!(
            header["kid"]
                .as_str()
                .is_some_and(|k| k.starts_with("agent-escurel-")),
            "the kid must be in the platform's scheme, or every token is \
             rejected on a kid miss: {header}"
        );
        assert_eq!(claims["tenant"], "datazoo-loops", "{claims}");
        assert_eq!(claims["aud"], "escurel", "{claims}");
        assert_eq!(claims["sub"], "escurel-runner", "{claims}");
        assert_eq!(claims["roles"][0], "escurel:admin", "{claims}");
        assert!(
            claims["exp"].as_u64().unwrap_or(0) > claims["iat"].as_u64().unwrap_or(0),
            "{claims}"
        );
    }

    #[test]
    fn a_delegation_token_is_agent_scoped_carries_no_escurel_authority() {
        let signer = Signer::build(
            "https://agent-lab.data-zoo.de".into(),
            "escurel".into(), // escurel's OWN audience
            "default".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let token = signer
            .mint_delegation(
                "escurel-async-runner",
                "agent-a2a",
                "msteams:29:alice",
                "01STEP",
                120,
            )
            .expect("mint delegation");

        let parts: Vec<&str> = token.split('.').collect();
        let decode = |s: &str| {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .expect("b64");
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
        };
        let claims = decode(parts[1]);

        // Agent-scoped, NOT escurel's own audience — never interchangeable.
        assert_eq!(claims["aud"], "agent-a2a", "{claims}");
        assert_ne!(
            claims["aud"], "escurel",
            "must not carry escurel's audience"
        );
        // No escurel authority whatsoever.
        assert_eq!(
            claims["roles"],
            serde_json::json!([]),
            "a delegation token carries NO roles: {claims}"
        );
        // The purpose the gateway verifier rejects.
        assert_eq!(claims[PURPOSE_CLAIM], DELEGATION_PURPOSE, "{claims}");
        // Requester rides as audit-only obo; step + jti + nbf bound it.
        assert_eq!(claims["sub"], "escurel-async-runner", "{claims}");
        assert_eq!(claims["obo"], "msteams:29:alice", "{claims}");
        assert_eq!(claims["step"], "01STEP", "{claims}");
        assert!(
            claims["jti"].as_str().is_some_and(|j| !j.is_empty()),
            "{claims}"
        );
        assert!(claims["nbf"].as_u64().is_some(), "{claims}");
        assert!(
            claims["exp"].as_u64().unwrap_or(0) > claims["iat"].as_u64().unwrap_or(0),
            "{claims}"
        );
    }

    /// The reason to mint at all: a token that is about to lapse is replaced
    /// before it is handed out.
    #[test]
    fn a_stale_cached_token_is_re_minted() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "t".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let stale = signer.mint("escurel-runner", 60).expect("mint");
        let source = TokenSource::Minted {
            signer,
            ttl_secs: TTL_SECS,
            subject: "escurel-runner".into(),
            // Expires inside the refresh margin: still valid, not valid
            // enough to hand to a run that may take minutes.
            cached: Mutex::new(Some((stale.clone(), now_secs() + 60))),
        };

        let fresh = source.current().expect("current");
        assert_ne!(
            fresh, stale,
            "a token inside the refresh margin must be re-minted"
        );
        // Positive control: the freshly minted one IS reused, so the test
        // above is about staleness and not about caching being broken.
        assert_eq!(source.current().expect("current"), fresh);
    }

    #[test]
    fn a_static_token_is_returned_unchanged() {
        let source = TokenSource::Static("pasted-bearer".into());
        assert_eq!(source.current().expect("current"), "pasted-bearer");
    }

    #[test]
    fn only_a_minting_source_delegates_and_it_scopes_to_the_agent(// A minting runner mints a delegation for the delegate harness; a static
        // one cannot and returns None, so a delegate step fails closed rather
        // than presenting the runner's own bearer to the agent (Phase 4 3c).
    ) {
        let signer = Signer::build(
            "https://agent-lab.data-zoo.de".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let minted = TokenSource::Minted {
            signer,
            ttl_secs: 120,
            subject: "escurel-async-runner".into(),
            cached: Mutex::new(None),
        };

        let token = minted
            .mint_delegation("agent-a2a", "msteams:29:alice", "01STEP")
            .expect("mint")
            .expect("a minting source delegates");
        let claims = {
            let parts: Vec<&str> = token.split('.').collect();
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .expect("b64");
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
        };
        assert_eq!(claims["aud"], "agent-a2a", "agent-scoped: {claims}");
        assert_ne!(claims["aud"], "escurel", "never escurel's own audience");
        assert_eq!(claims["roles"], serde_json::json!([]), "no authority");
        assert_eq!(claims[PURPOSE_CLAIM], DELEGATION_PURPOSE);
        assert_eq!(
            claims["sub"], "escurel-async-runner",
            "runner is the subject"
        );
        assert_eq!(claims["obo"], "msteams:29:alice", "requester is obo");
        assert_eq!(claims["step"], "01STEP");

        // A static-bearer source holds no key → cannot delegate → None.
        let static_src = TokenSource::Static("pasted-bearer".into());
        assert!(
            static_src
                .mint_delegation("agent-a2a", "x", "y")
                .expect("no error")
                .is_none(),
            "a static source cannot mint a delegation; the delegate step fails closed"
        );
    }

    /// The claims of a JWT, without verifying it — these tests own the key.
    fn claims_of(token: &str) -> serde_json::Value {
        let payload = token.split('.').nth(1).expect("a JWT has three parts");
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .expect("b64");
        serde_json::from_slice(&bytes).expect("json")
    }

    /// #510: two runs on different skills must not both write as the runner.
    /// The minted per-run token names the AGENT as `sub` — that is what the
    /// gateway stamps into `last_written_by` — and keeps the runner visible as
    /// the delegating actor in `act.sub`, so "runner acting as inbox-scan" is
    /// recoverable from the token alone.
    #[test]
    fn a_per_run_agent_token_names_the_agent_and_keeps_the_runner_as_actor() {
        let signer = Signer::build(
            "https://agent-lab.data-zoo.de".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");

        let token = signer
            .mint_agent("escurel-runner", "inbox-scan", 120, None)
            .expect("mint agent");
        let claims = claims_of(&token);

        assert_eq!(
            claims["sub"], "agent:inbox-scan",
            "the agent is the subject, so last_written_by names it: {claims}"
        );
        assert_eq!(
            claims["act"]["sub"], "escurel-runner",
            "the delegation chain to the runner stays recoverable: {claims}"
        );
        // Unchanged from today: same tenant, audience and authority, so this
        // is plumbing, not a behaviour change (#510 proposal step 2).
        assert_eq!(claims["tenant"], "acme", "{claims}");
        assert_eq!(claims["aud"], "escurel", "{claims}");
        assert_eq!(claims["roles"][0], "escurel:admin", "{claims}");
        assert_eq!(
            claims["exp"].as_u64().unwrap_or(0) - claims["iat"].as_u64().unwrap_or(0),
            120,
            "exp is bounded by the run budget handed in, not the process lifetime: {claims}"
        );
    }

    /// The whole point: two skills, two identities.
    #[test]
    fn two_skills_mint_two_distinct_subjects() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");

        let inbox = claims_of(
            &signer
                .mint_agent("escurel-runner", "inbox-scan", 60, None)
                .expect("a"),
        );
        let hygiene = claims_of(
            &signer
                .mint_agent("escurel-runner", "crm-hygiene", 60, None)
                .expect("b"),
        );

        assert_ne!(
            inbox["sub"], hygiene["sub"],
            "two agents sharing one identity is the bug: {inbox} vs {hygiene}"
        );
        assert_eq!(inbox["act"]["sub"], hygiene["act"]["sub"], "same runner");
    }

    /// A skill id is a page id, not a claim: anything that could make the
    /// subject ambiguous is rejected rather than silently normalised into a
    /// collision with another agent's identity.
    #[test]
    fn an_unusable_skill_id_does_not_mint_an_ambiguous_subject() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");

        for bad in ["", "   ", "has space", "with:colon", "../escalate"] {
            assert!(
                signer.mint_agent("escurel-runner", bad, 60, None).is_err(),
                "{bad:?} must not mint a subject"
            );
        }
    }

    /// A static-bearer runner holds a bearer, not a key — it cannot scope a
    /// run to its agent and must say so rather than pretending.
    #[test]
    fn only_a_minting_source_scopes_a_run_to_its_agent() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let minted = TokenSource::Minted {
            signer,
            ttl_secs: 120,
            subject: "escurel-runner".into(),
            cached: Mutex::new(None),
        };

        let token = minted
            .mint_agent("inbox-scan", 90, None)
            .expect("mint")
            .expect("a minting source scopes the run");
        let claims = claims_of(&token);
        assert_eq!(claims["sub"], "agent:inbox-scan", "{claims}");
        assert_eq!(claims["act"]["sub"], "escurel-runner", "{claims}");

        assert!(
            TokenSource::Static("pasted-bearer".into())
                .mint_agent("inbox-scan", 90, None)
                .expect("no error")
                .is_none(),
            "a static source cannot scope a run; the caller falls back to the runner"
        );
    }

    fn run() -> RunClaims {
        RunClaims {
            run_id: "01HRUN".into(),
            root_event_id: "01HROOT".into(),
            trace_id: Some("0123456789abcdef0123456789abcdef".into()),
        }
    }

    /// The run's identity rides ON the token (workbench backend P1): the
    /// gateway stamps a draft's `run_id` / `root_event_id` from it and
    /// authorises `report_progress` by it, so it must be unforgeable by the
    /// harness — a claim the runner signed, not a header the agent sends.
    #[test]
    fn mint_agent_carries_run_claims_when_given() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let minted = TokenSource::Minted {
            signer,
            ttl_secs: 120,
            subject: "escurel-runner".into(),
            cached: Mutex::new(None),
        };
        let token = minted
            .mint_agent("inbox-scan", 90, Some(&run()))
            .expect("mint")
            .expect("minting source");
        let claims = claims_of(&token);
        assert_eq!(claims["sub"], "agent:inbox-scan", "{claims}");
        assert_eq!(claims[RUN_ID_CLAIM], "01HRUN", "{claims}");
        assert_eq!(claims[ROOT_EVENT_ID_CLAIM], "01HROOT", "{claims}");
        assert_eq!(
            claims[TRACE_ID_CLAIM], "0123456789abcdef0123456789abcdef",
            "{claims}"
        );
        // Without a run (recovery, a bare mint) the claims are simply absent.
        let bare = minted
            .mint_agent("inbox-scan", 90, None)
            .expect("mint")
            .expect("minting source");
        assert!(claims_of(&bare).get(RUN_ID_CLAIM).is_none());
    }

    /// A workflow run's requester-scoped token carries the run too, and the
    /// run claims never smuggle a privileged role back in.
    #[test]
    fn mint_scoped_carries_run_claims_and_still_strips_privileged_roles() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let minted = TokenSource::Minted {
            signer,
            ttl_secs: 120,
            subject: "escurel-runner".into(),
            cached: Mutex::new(None),
        };
        let token = minted
            .mint_scoped(
                "alice",
                &["team-acme".to_owned(), "escurel:admin".to_owned()],
                Some(&run()),
            )
            .expect("mint")
            .expect("minting source");
        let claims = claims_of(&token);
        assert_eq!(claims["sub"], "alice");
        assert_eq!(claims[RUN_ID_CLAIM], "01HRUN", "{claims}");
        assert_eq!(claims[ROOT_EVENT_ID_CLAIM], "01HROOT", "{claims}");
        assert_eq!(
            claims["roles"],
            serde_json::json!(["team-acme"]),
            "{claims}"
        );
    }

    /// Never cached (unlike [`TokenSource::current`]): a per-run token that
    /// outlived its run would re-introduce exactly the frozen-bearer defect
    /// #442 recorded.
    #[test]
    fn a_per_run_token_is_minted_fresh_every_time() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "acme".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let minted = TokenSource::Minted {
            signer,
            ttl_secs: 120,
            subject: "escurel-runner".into(),
            cached: Mutex::new(None),
        };

        let first = minted
            .mint_agent("inbox-scan", 90, None)
            .expect("a")
            .expect("a");
        let second = minted
            .mint_agent("inbox-scan", 90, None)
            .expect("b")
            .expect("b");
        assert_ne!(
            claims_of(&first)["jti"],
            serde_json::Value::Null,
            "a per-run token carries a jti so two runs are distinguishable"
        );
        assert_ne!(
            claims_of(&first)["jti"],
            claims_of(&second)["jti"],
            "each run mints its own, never a cached one"
        );
    }

    /// Neither the key nor a live bearer may reach a log.
    #[test]
    fn debug_redacts_the_credential() {
        let dbg = format!("{:?}", TokenSource::Static("super-secret".into()));
        assert!(!dbg.contains("super-secret"), "{dbg}");
    }
}
