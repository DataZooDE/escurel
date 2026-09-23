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

use base64::Engine as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// The signing identity and the claim shape live in `escurel-auth` (one
/// home for what the gateway verifies and what anyone mints); the runner's
/// public API keeps the same names.
pub use escurel_auth::{
    MintRunClaims as RunClaims, ROOT_EVENT_ID_CLAIM, RUN_ID_CLAIM, SignError, Signer,
    TRACE_ID_CLAIM,
};

/// How long a minted bearer lives.
///
/// Long enough that a run and its retries never straddle an expiry; short
/// enough that a leaked one is worth little. Re-minted well before it lapses
/// — see [`REFRESH_MARGIN_SECS`].
pub const TTL_SECS: u64 = 30 * 60;

/// Re-mint this long before expiry, so a token is never handed out with less
/// life left than a slow run might need.
const REFRESH_MARGIN_SECS: u64 = 5 * 60;

/// Errors from building or using the runner's credential.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The signing identity could not be built or could not sign
    /// (`ESCUREL_RUNNER_AUTH_SIGNING_KEY` and friends).
    #[error("{0}")]
    Sign(#[from] SignError),
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
    ///
    /// `narrow`: `Some(groups)` mints the token NARROWED to the target skill
    /// (`escurel:agent` + the skill's write groups, P3-6) instead of the
    /// runner's admin authority; `None` keeps the admin grant (the default,
    /// `ESCUREL_RUNNER_AGENT_NARROW` off).
    pub fn mint_agent(
        &self,
        label_skill: &str,
        ttl_secs: u64,
        run: Option<&RunClaims>,
        narrow: Option<&[String]>,
    ) -> Result<Option<String>, AuthError> {
        match self {
            Self::Static(_) => Ok(None),
            Self::Minted {
                signer, subject, ..
            } => Ok(Some(match narrow {
                Some(groups) => {
                    signer.mint_agent_narrowed(subject, label_skill, ttl_secs, run, groups)?
                }
                None => signer.mint_agent(subject, label_skill, ttl_secs, run)?,
            })),
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use escurel_auth::{DELEGATION_PURPOSE, PURPOSE_CLAIM};
    use rsa::RsaPrivateKey;

    /// A real 2048-bit RSA key, generated for this test.
    fn test_key() -> String {
        use rsa::pkcs8::EncodePrivateKey;
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("generate");
        key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("encode")
            .to_string()
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
            .mint_agent("inbox-scan", 90, None, None)
            .expect("mint")
            .expect("a minting source scopes the run");
        let claims = claims_of(&token);
        assert_eq!(claims["sub"], "agent:inbox-scan", "{claims}");
        assert_eq!(claims["act"]["sub"], "escurel-runner", "{claims}");

        assert!(
            TokenSource::Static("pasted-bearer".into())
                .mint_agent("inbox-scan", 90, None, None)
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
            .mint_agent("inbox-scan", 90, None, None)
            .expect("a")
            .expect("a");
        let second = minted
            .mint_agent("inbox-scan", 90, None, None)
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
            .mint_agent("inbox-scan", 90, Some(&run()), None)
            .expect("mint")
            .expect("minting source");
        let claims = claims_of(&token);
        assert_eq!(claims["sub"], "agent:inbox-scan", "{claims}");
        assert_eq!(claims["roles"], serde_json::json!(["escurel:admin"]));
        // Narrowed (P3-6): agent + the skill's groups, the run claims kept.
        let narrowed = minted
            .mint_agent("inbox-scan", 90, Some(&run()), Some(&["ops".to_owned()]))
            .expect("mint")
            .expect("minting source");
        let claims = claims_of(&narrowed);
        assert_eq!(claims["roles"], serde_json::json!(["escurel:agent", "ops"]));
        assert_eq!(claims[RUN_ID_CLAIM], "01HRUN", "{claims}");
        assert_eq!(claims[RUN_ID_CLAIM], "01HRUN", "{claims}");
        assert_eq!(claims[ROOT_EVENT_ID_CLAIM], "01HROOT", "{claims}");
        assert_eq!(
            claims[TRACE_ID_CLAIM], "0123456789abcdef0123456789abcdef",
            "{claims}"
        );
        // Without a run (recovery, a bare mint) the claims are simply absent.
        let bare = minted
            .mint_agent("inbox-scan", 90, None, None)
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
}
