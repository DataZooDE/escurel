//! The RSA signing identity that mints escurel bearers.
//!
//! Lived in `escurel-runner-core` while only the runner minted (its own
//! service bearer, a per-run scoped token, a per-run agent token, an A2A
//! delegation). The gateway mints too now — `mint_agent_token`, the
//! workbench's way to hand an interactive agent a run-bound token
//! (knowledge-workbench backend P2-6) — and the verifier that reads these
//! claims lives here, so the claim shape has ONE home. The runner keeps
//! [`TokenSource`](https://docs.rs/escurel-runner-core) and re-exports this.
//!
//! The claim shape and the `kid` derivation are deliberately IDENTICAL to
//! the ones heron and `agent-core` use: this borrows the platform's existing
//! signing identity rather than becoming a second issuer, so the JWKS the
//! platform already publishes verifies these tokens.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;

/// The claim the gateway's verifier reads for the tenant.
pub const TENANT_CLAIM: &str = "tenant";

/// The `purpose` claim on an internal-delegation token (fleet #801 Phase 4). A
/// token the runner presents to the AGENT when delegating a domain step — NOT an
/// escurel token. Shared so the escurel gateway verifier can REJECT a token
/// carrying it (a delegation bearer must never be accepted back into escurel's
/// own `/mcp`).
pub const DELEGATION_PURPOSE: &str = "internal_delegation";
/// The claim key under which [`DELEGATION_PURPOSE`] is carried.
pub const PURPOSE_CLAIM: &str = "purpose";

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
pub enum SignError {
    /// The key did not parse as PKCS#8 or PKCS#1 RSA.
    #[error("the signing key is not a valid RSA private key (tried PKCS#8, PKCS#1)")]
    KeyParse,
    /// Re-encoding the parsed key failed.
    #[error("re-encoding the RSA key failed: {0}")]
    KeyEncode(rsa::pkcs1::Error),
    /// Signing failed.
    #[error("minting failed: {0}")]
    Mint(#[from] jsonwebtoken::errors::Error),
    /// The run's `label_skill` cannot become an unambiguous token subject.
    #[error(
        "{0:?} is not usable as a per-run agent subject (expected a skill id of \
         letters, digits, '_', '-')"
    )]
    UnusableAgentSubject(String),
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
    ) -> Result<Self, SignError> {
        let raw = signing_key_pem.trim();
        let private = RsaPrivateKey::from_pkcs8_pem(raw)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(raw))
            .map_err(|_| SignError::KeyParse)?;
        let private_pem = private
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .map_err(SignError::KeyEncode)?
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
    pub fn mint(&self, subject: &str, ttl_secs: u64) -> Result<String, SignError> {
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
    ) -> Result<String, SignError> {
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
    ) -> Result<String, SignError> {
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
    ) -> Result<String, SignError> {
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
    ) -> Result<String, SignError> {
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
fn agent_slug(label_skill: &str) -> Result<&str, SignError> {
    let ok = !label_skill.is_empty()
        && !label_skill.contains("..")
        && label_skill
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if ok {
        Ok(label_skill)
    } else {
        Err(SignError::UnusableAgentSubject(label_skill.to_owned()))
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
}
