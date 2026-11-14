//! `OidcVerifier`: validate a bearer JWT against the configured
//! issuer and project it into an [`AuthContext`].

use std::time::Duration;

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;

use crate::jwks::{JwksCache, JwksCacheError};

/// Operator-supplied OIDC configuration. Mirrors
/// `docs/spec/platform.md §Auth`.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// `https://auth.example.com/realms/main`. The verifier
    /// appends `/protocol/openid-connect/certs` only when no
    /// explicit `jwks_uri` is supplied via [`OidcConfig::jwks_uri`].
    pub issuer: String,
    pub audience: String,
    /// JWT claim name that carries the tenant id. Default `"tenant"`.
    pub tenant_claim: String,
    /// JWT claim name that lists the subject's roles. Default
    /// `"roles"`.
    pub admin_role_claim: String,
    /// The role value that grants admin access. Default
    /// `"escurel:admin"`.
    pub admin_role_value: String,
    /// The role value (in the same claim as [`Self::admin_role_value`]) that marks the
    /// credential as an OPERATOR: the only credential that may act on a tenant other than
    /// its own. Default `"escurel:operator"`. An operator is also an admin.
    pub operator_role_value: String,
    /// The role value (same claim as admin) that marks a RUNNER service
    /// credential (#828): it may claim inbox events, keep the runner's books
    /// and mint per-run tokens, and nothing an admin alone may do. Default
    /// `"escurel:runner"`.
    pub runner_role_value: String,
    /// The role value (same claim as admin) that marks a chat BRIDGE service
    /// credential. Default `"escurel:bridge"`.
    pub bridge_role_value: String,
    /// JWT claim name that lists the subject's group/role memberships
    /// for the data-level ACL. Default `"roles"` (the same claim admin
    /// derives from). Parsed leniently — see [`parse_groups_claim`].
    pub groups_claim: String,
    /// TTL for the in-memory JWKS cache.
    pub jwks_refresh: Duration,
    /// Optional override for the JWKS URL. When `None`, the
    /// verifier constructs `${issuer}/protocol/openid-connect/certs`
    /// (the Keycloak convention).
    pub jwks_uri: Option<String>,
    /// Additional trusted issuers beyond the primary [`Self::issuer`],
    /// each an `(issuer, jwks_uri_override)` pair. Empty by default →
    /// single-issuer behaviour. The substrate uses one entry here so a
    /// single Escurel trusts both Triton (the forwarded inbound bearer)
    /// and Carl (the self-minted dashboard token); both name the same
    /// [`Self::audience`], differing only in `iss` + signing key.
    pub additional_issuers: Vec<(String, Option<String>)>,
    /// The trusted issuer that is a chat-channel bridge
    /// (`ESCUREL_AUTH_BRIDGE_ISSUER`). EVERY token verified against this
    /// entry is a BRIDGE token, restricted whatever it claims or whichever
    /// kid it names — see [`OidcVerifier::verify`] and `escurel_types::bridge`.
    /// The restriction belongs to the trust entry, not to a kid: the bridge
    /// serves this entry's JWKS, so whoever controls that response can publish
    /// any kid it likes.
    pub bridge_issuer: Option<String>,
    /// Key ids the bridge signs under (`ESCUREL_AUTH_BRIDGE_KID`): an extra
    /// pin on the bridge entry — a token from it under any other kid is
    /// refused. A token from another entry under one of these kids is refused
    /// too (fail closed). The key must be the bridge's own, never the
    /// gateway's main signing key.
    pub bridge_kids: Vec<String>,
    /// The one tenant a bridge token may name (`ESCUREL_AUTH_BRIDGE_TENANT`);
    /// a bridge token naming another is refused. `None` = not pinned here.
    pub bridge_tenant: Option<String>,
    /// The gateway's OWN signing identity, `(issuer, kid)`: the only one
    /// whose bridge TURN tokens (`mint_user_token`, #830) are honoured with
    /// their groups. A `bridge_channel` token from any other trusted entry —
    /// another IdP, a runner key, a bridge's old `ISSUER_<N>` left in place —
    /// is refused (crew S-F6). `None` = this gateway does not mint turns.
    pub minting_identity: Option<(String, String)>,
}

impl OidcConfig {
    /// Defaults matching the substrate binding
    /// (`docs/deploy/substrate.md §1`).
    #[must_use]
    pub fn new(issuer: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            tenant_claim: "tenant".to_owned(),
            admin_role_claim: "roles".to_owned(),
            admin_role_value: "escurel:admin".to_owned(),
            operator_role_value: "escurel:operator".to_owned(),
            runner_role_value: "escurel:runner".to_owned(),
            bridge_role_value: "escurel:bridge".to_owned(),
            groups_claim: "roles".to_owned(),
            jwks_refresh: Duration::from_secs(300),
            jwks_uri: None,
            additional_issuers: Vec::new(),
            bridge_issuer: None,
            minting_identity: None,
            bridge_kids: Vec::new(),
            bridge_tenant: None,
        }
    }

    /// Pin the bridge entry to `tenant` (see [`Self::bridge_tenant`]).
    #[must_use]
    pub fn with_bridge_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.bridge_tenant = Some(tenant.into());
        self
    }

    /// The gateway's own signing `(issuer, kid)` (see [`Self::minting_identity`]).
    #[must_use]
    pub fn with_minting_identity(
        mut self,
        issuer: impl Into<String>,
        kid: impl Into<String>,
    ) -> Self {
        self.minting_identity = Some((issuer.into(), kid.into()));
        self
    }

    /// Mark the trusted issuer `issuer` as a chat bridge: every token
    /// verified against it is restricted. It must also be configured as the
    /// primary or an additional issuer.
    #[must_use]
    pub fn with_bridge_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.bridge_issuer = Some(issuer.into());
        self
    }

    /// Pin the bridge entry to `kid` (see [`Self::bridge_kids`]).
    #[must_use]
    pub fn with_bridge_kid(mut self, kid: impl Into<String>) -> Self {
        self.bridge_kids.push(kid.into());
        self
    }

    #[must_use]
    pub fn with_jwks_uri(mut self, uri: impl Into<String>) -> Self {
        self.jwks_uri = Some(uri.into());
        self
    }

    /// Trust one more issuer beyond the primary, with an optional
    /// explicit JWKS URL (derived from the issuer when `None`). The
    /// added issuer shares the primary's audience, tenant claim, and
    /// role config.
    #[must_use]
    pub fn with_additional_issuer(
        mut self,
        issuer: impl Into<String>,
        jwks_uri: Option<String>,
    ) -> Self {
        self.additional_issuers.push((issuer.into(), jwks_uri));
        self
    }

    #[must_use]
    pub fn with_tenant_claim(mut self, claim: impl Into<String>) -> Self {
        self.tenant_claim = claim.into();
        self
    }

    #[must_use]
    pub fn with_admin_role(mut self, claim: impl Into<String>, value: impl Into<String>) -> Self {
        self.admin_role_claim = claim.into();
        self.admin_role_value = value.into();
        self
    }

    /// Rename the role value that marks an operator credential.
    #[must_use]
    pub fn with_operator_role(mut self, value: impl Into<String>) -> Self {
        self.operator_role_value = value.into();
        self
    }

    /// Rename the role values that mark the runner and bridge service
    /// credentials (see [`Self::runner_role_value`]).
    #[must_use]
    pub fn with_service_roles(
        mut self,
        runner: impl Into<String>,
        bridge: impl Into<String>,
    ) -> Self {
        self.runner_role_value = runner.into();
        self.bridge_role_value = bridge.into();
        self
    }

    #[must_use]
    pub fn with_groups_claim(mut self, claim: impl Into<String>) -> Self {
        self.groups_claim = claim.into();
        self
    }

    fn effective_jwks_uri(&self) -> String {
        derive_jwks_uri(&self.issuer, self.jwks_uri.as_deref())
    }
}

/// JWKS URL for an issuer: the explicit override, else the Keycloak
/// convention `${issuer}/protocol/openid-connect/certs`.
fn derive_jwks_uri(issuer: &str, override_uri: Option<&str>) -> String {
    override_uri.map(ToOwned::to_owned).unwrap_or_else(|| {
        format!(
            "{}/protocol/openid-connect/certs",
            issuer.trim_end_matches('/')
        )
    })
}

/// Resolved auth result. Downstream layers route on these three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub subject: String,
    pub tenant_id: String,
    pub role: Role,
    /// The credential carries the operator role value: it may act on a tenant other than the
    /// one in its `tenant` claim (cross-tenant administration). `role` is `Admin` whenever this
    /// is set. Authorization-relevant: an admin that is not an operator is confined to its own
    /// tenant.
    pub operator: bool,
    /// Group/role names parsed from the configured `groups_claim`.
    /// Raw — reserved-name and admin-value stripping is the ACL
    /// layer's job (`escurel-index`), not the verifier's.
    pub groups: Vec<String>,
    /// Who this subject is acting FOR, from the RFC 8693 `act.sub` claim
    /// (#510): a per-run agent token carries the runner that delegated to it,
    /// so the chain "runner acting as inbox-scan" survives verification and
    /// can be stamped into provenance. `None` for an ordinary token — the
    /// subject is acting as itself.
    ///
    /// The run this token was minted for (`run_id` claim, non-empty), with
    /// its lineage root and trace when present; `None` for a token that
    /// belongs to no run — an ordinary bearer, or a runner's own.
    pub run: Option<RunClaims>,
    /// Authorization NEVER reads this: the authority is the subject's own
    /// (`role`, `groups`). It is audit lineage, not a capability.
    pub actor: Option<String>,
    /// The skill a NARROWED per-run agent token is confined to (`skill`
    /// claim, workbench backend P3-6). Unlike `run` and `actor` this IS an
    /// authorization input — a restriction, never a grant: the write ACL
    /// refuses an instance write under any other skill. `None` for every
    /// other token.
    pub agent_skill: Option<String>,
    /// The agent profile a per-run token runs as (`agent` claim, #823). An
    /// authorization input, and only ever a restriction: the gateway loads the
    /// named `agent` page itself and enforces its scopes, refusing every tool
    /// when the page is missing. A claim that is present but not a non-empty
    /// string is kept as `Some("")` — it names no agent the gateway can find,
    /// so it fails closed rather than reading as "no agent".
    pub agent: Option<String>,
    /// The token was verified against the chat-bridge trust entry
    /// ([`OidcConfig::bridge_issuer`]). An authorization input, and only ever
    /// a restriction: the gateway confines such a context to the few calls a
    /// bridge makes (`escurel-server`'s bridge gate). A TURN token the bridge
    /// minted for one person's message — not the bridge's own service
    /// credential, which is [`ServiceRole::Bridge`] in [`Self::service`].
    pub bridge_turn: bool,
    /// The built-in service role this credential holds (#828), from the
    /// configured runner / bridge role values. `None` for every machine token
    /// (one carrying `run_id`, `skill`, `agent`, `act` or a workbench purpose):
    /// a service role is a property of a service's OWN credential, so a token
    /// minted for a run never inherits it, whatever its roles say.
    pub service: Option<ServiceRole>,
    /// The token's `iat` (issued-at, Unix seconds) when it carries a numeric
    /// one. Compared against an agent page's `token_epoch` to revoke the
    /// tokens issued for that agent before it (#829).
    pub issued_at: Option<u64>,
    /// The person a gateway-minted bridge turn acts for when its subject is
    /// a GROUP principal (a mapped Teams group chat or channel, #830): the
    /// token's `act.sub`. Audit lineage only — the authority is the group's
    /// — stamped as `asked_by` on what the turn captures. `None` otherwise.
    pub asker: Option<String>,
}

pub use escurel_types::service_role::ServiceRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Standard agent-level access. No admin endpoints.
    Agent,
    /// Admin role grants tenant-CRUD and operator endpoints.
    Admin,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("token header could not be decoded: {0}")]
    BadHeader(String),
    #[error("token header missing required `kid`")]
    MissingKid,
    #[error("JWKS lookup failed: {0}")]
    Jwks(#[from] JwksCacheError),
    #[error("token validation failed: {0}")]
    Invalid(String),
    #[error("token header algorithm `{0:?}` is not in the trusted allow-list")]
    UnsupportedAlg(Algorithm),
    #[error("token missing required `{tenant_claim}` claim")]
    MissingTenant { tenant_claim: String },
    #[error("token carries purpose `{0}`, which is not accepted at this surface")]
    PurposeRefused(String),
    #[error("bridge token refused: {0}")]
    BridgeRefused(String),
}

/// The `purpose` claim + value on a runner→agent INTERNAL-DELEGATION token
/// (fleet #801 AD-7). Kept in lock-step with
/// `escurel_runner_core::auth::{PURPOSE_CLAIM, DELEGATION_PURPOSE}` — escurel-auth
/// must not depend on runner-core (the dependency runs the other way), so the
/// strings are duplicated here with this note.
/// RFC 8693 §4.1 delegation claim: who the subject is acting for. Read for
/// audit lineage only — never for an authorization decision (#510).
const ACT_CLAIM: &str = "act";

use crate::signer::{
    DELEGATION_PURPOSE, PURPOSE_CLAIM as DELEGATION_PURPOSE_CLAIM, WORKBENCH_AGENT_PURPOSE,
};

/// The run-identity claims a runner mints onto a per-run token (workbench
/// backend P1). Kept in lock-step with
/// `escurel_runner_core::auth::{RUN_ID_CLAIM, ROOT_EVENT_ID_CLAIM, TRACE_ID_CLAIM}`
/// — same dependency direction as the delegation constants above.
use crate::signer::{AGENT_CLAIM, ROOT_EVENT_ID_CLAIM, RUN_ID_CLAIM, SKILL_CLAIM, TRACE_ID_CLAIM};

/// The run a per-run token belongs to, as the runner minted it: the ledger
/// run id, the lineage root event, and the lineage's trace when there is
/// one. Read to stamp lineage onto what the run writes and to authorise
/// `report_progress` for exactly this run — never as an authorization input
/// for anything else (the authority is the subject's own, like `actor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunClaims {
    pub run_id: String,
    pub root_event_id: Option<String>,
    pub trace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    #[serde(default, flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// One trusted issuer and the JWKS cache that backs it. The verifier
/// holds one per configured issuer and routes a token to the entry
/// whose `issuer` matches the token's `iss`.
struct TrustEntry {
    issuer: String,
    jwks: JwksCache,
    /// This entry is the chat bridge's ([`OidcConfig::bridge_issuer`]).
    bridge: bool,
}

pub struct OidcVerifier {
    config: OidcConfig,
    /// Primary issuer first, then any additional issuers. Non-empty.
    entries: Vec<TrustEntry>,
}

impl std::fmt::Debug for OidcVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let issuers: Vec<&str> = self.entries.iter().map(|e| e.issuer.as_str()).collect();
        f.debug_struct("OidcVerifier")
            .field("issuers", &issuers)
            .field("audience", &self.config.audience)
            .finish_non_exhaustive()
    }
}

/// Just the `iss` claim — read WITHOUT signature verification, only to
/// route the token to its trust entry. The verified decode below is
/// what authenticates; this projection is never trusted on its own.
#[derive(Debug, Deserialize)]
struct IssProbe {
    iss: Option<String>,
}

impl OidcVerifier {
    /// Build a verifier from config. Lazy: the JWKS isn't fetched
    /// until the first `verify` call (or an explicit
    /// `prime_cache`).
    #[must_use]
    pub fn new(config: OidcConfig) -> Self {
        let mut entries = Vec::with_capacity(1 + config.additional_issuers.len());
        let is_bridge = |issuer: &str| config.bridge_issuer.as_deref() == Some(issuer);
        // Primary issuer first.
        entries.push(TrustEntry {
            issuer: config.issuer.clone(),
            jwks: JwksCache::new(config.effective_jwks_uri(), config.jwks_refresh),
            bridge: is_bridge(&config.issuer),
        });
        for (issuer, jwks_uri) in &config.additional_issuers {
            entries.push(TrustEntry {
                issuer: issuer.clone(),
                jwks: JwksCache::new(
                    derive_jwks_uri(issuer, jwks_uri.as_deref()),
                    config.jwks_refresh,
                ),
                bridge: is_bridge(issuer),
            });
        }
        Self { config, entries }
    }

    /// Force a JWKS fetch now for every trusted issuer (e.g. to
    /// surface JWKS errors at startup instead of on the first request).
    pub async fn prime_cache(&self) -> Result<(), AuthError> {
        for entry in &self.entries {
            entry.jwks.refresh().await?;
        }
        Ok(())
    }

    /// Verify a bearer token. Returns an [`AuthContext`] on
    /// success.
    pub async fn verify(&self, token: &str) -> Result<AuthContext, AuthError> {
        let header = decode_header(token).map_err(|e| AuthError::BadHeader(e.to_string()))?;
        // Pin the accepted algorithm set from a trusted allow-list
        // (asymmetric only — RSA/ECDSA, never HMAC) rather than
        // trusting the header. Reject the untrusted header `alg` if
        // it falls outside the allow-list before doing any further
        // work. Defence-in-depth against alg-confusion / downgrade.
        let alg = header.alg;
        if !allowed_algorithms().contains(&alg) {
            return Err(AuthError::UnsupportedAlg(alg));
        }
        let kid = header.kid.ok_or(AuthError::MissingKid)?;
        // Route by the token's `iss` to its trust entry. The issuer is
        // read WITHOUT verifying the signature — purely to pick which
        // configured JWKS + issuer to verify against. The verified
        // `decode` below (against that entry's published key) is what
        // authenticates; a forged `iss` only selects the wrong entry,
        // whose key then fails to verify the signature.
        let entry = self.select_entry(token)?;
        // The bridge kids pin the bridge entry, both ways, before any key is
        // fetched: the bridge serves this entry's JWKS, so an unlisted kid
        // there is a key the gateway never agreed to, and a listed kid under
        // another entry means the configuration is confused. Refused either
        // way — never verified as an unrestricted token.
        match (entry.bridge, self.config.bridge_kids.contains(&kid)) {
            (true, false) => {
                return Err(AuthError::BridgeRefused(format!(
                    "kid `{kid}` is not a bridge kid (ESCUREL_AUTH_BRIDGE_KID)"
                )));
            }
            (false, true) => {
                return Err(AuthError::BridgeRefused(format!(
                    "bridge kid `{kid}` under issuer `{}`, which is not the bridge issuer",
                    entry.issuer
                )));
            }
            _ => {}
        }
        let key = entry.jwks.key_for_kid(&kid).await?;
        // Validate against exactly the (already allow-listed) header
        // alg. We must not stuff the whole allow-list in here —
        // jsonwebtoken returns `InvalidAlgorithm` if any algorithm in
        // the set is incompatible with the key family (e.g. an EC alg
        // against an RSA key). The allow-list gate above is what bounds
        // the accepted set; `Validation::new(alg)` pins this token to
        // its single, vetted algorithm.
        let mut validation = Validation::new(alg);
        // **An expired bearer is expired.** `jsonwebtoken` defaults `leeway`
        // to 60 seconds, and inheriting that meant a token was accepted for a
        // full minute past its `exp`.
        //
        // Set explicitly, and set to zero, for two reasons. The credentials
        // this gateway sees are short-lived on purpose — a runner's minted
        // bearer lives 30 minutes and is re-minted before it lapses — so a
        // 60-second grace is a meaningful fraction of the window an expired
        // token is usable in, and every caller on this surface already knows
        // how to get a new one.
        //
        // The second reason is worse: leeway HIDES expiry bugs. A test that
        // minted a 5-second bearer, slept, and passed against a credential
        // path that provably never re-minted (escurel#442) was green only
        // because of this default — the same way the production bug it was
        // written for stayed invisible. A guard that makes broken
        // re-authentication look correct is worse than no guard.
        //
        // Clock skew is the thing this gives up, and it is the right thing to
        // give up here: the hosts are NTP-synced, the issuers are Google and
        // our own minter, and a caller refused a second early retries with a
        // fresh token rather than failing.
        validation.leeway = 0;
        validation.set_audience(&[self.config.audience.as_str()]);
        validation.set_issuer(&[entry.issuer.as_str()]);
        // Required claims left at default (exp, iat); we add aud + iss above.
        let token_data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AuthError::Invalid(e.to_string()))?;

        let claims = token_data.claims;
        if entry.bridge {
            return bridge_context(
                claims,
                &self.config.tenant_claim,
                self.config.bridge_tenant.as_deref(),
            );
        }
        // A bridge TURN token from an ordinary entry (#830): only the gateway's own mint, under its
        // own issuer and kid, carries a turn's groups. Any other entry's is a key being misused.
        if claims
            .rest
            .contains_key(escurel_types::bridge::CHANNEL_CLAIM)
        {
            let own = self
                .config
                .minting_identity
                .as_ref()
                .is_some_and(|(iss, k)| *iss == entry.issuer && *k == kid);
            if !own {
                return Err(AuthError::BridgeRefused(format!(
                    "a bridge turn token from issuer `{}` (kid `{kid}`), which is not this \
                     gateway's signing identity",
                    entry.issuer
                )));
            }
            return minted_bridge_context(claims, &self.config);
        }

        // Defense-in-depth (fleet #801 AD-7): a runner→agent internal-delegation
        // token is signed by the same issuer and verified against the same JWKS
        // as a gateway bearer, differing only by `aud` (the AGENT's, not
        // escurel's) and this `purpose`. The audience gate above already refuses
        // it, but reject the purpose explicitly and independently so a delegation
        // token can never authenticate at escurel's own surfaces even if a future
        // misconfiguration ever widened the accepted audience.
        if claims
            .rest
            .get(DELEGATION_PURPOSE_CLAIM)
            .and_then(serde_json::Value::as_str)
            == Some(DELEGATION_PURPOSE)
        {
            return Err(AuthError::PurposeRefused(DELEGATION_PURPOSE.to_owned()));
        }

        let tenant_id = claims
            .rest
            .get(&self.config.tenant_claim)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AuthError::MissingTenant {
                tenant_claim: self.config.tenant_claim.clone(),
            })?
            .to_owned();

        // A token minted FOR AN AGENT is never admin or operator, whatever its roles say: its groups
        // come from a skill page any member may edit, so in a deployment that maps an IdP group to
        // admin (`ESCUREL_AUTH_ADMIN_ROLE_VALUE`), a page listing that group would mint an admin agent.
        let agent_minted = claims.rest.contains_key(AGENT_CLAIM)
            || claims
                .rest
                .get(SKILL_CLAIM)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| !s.is_empty())
            || claims
                .rest
                .get(DELEGATION_PURPOSE_CLAIM)
                .and_then(serde_json::Value::as_str)
                == Some(WORKBENCH_AGENT_PURPOSE);
        let operator = !agent_minted
            && has_role_value(
                &claims.rest,
                &self.config.admin_role_claim,
                &self.config.operator_role_value,
            );
        let role = if !agent_minted && (operator || has_admin_role(&claims.rest, &self.config)) {
            Role::Admin
        } else {
            Role::Agent
        };

        let groups = claims
            .rest
            .get(&self.config.groups_claim)
            .map(parse_groups_claim)
            .unwrap_or_default();

        // RFC 8693 §4.1 `act`: a nested {"sub": …}. Anything else is a claim we
        // do not understand — no actor, and not an error: a token is not
        // invalid for carrying something extra.
        let actor = claims
            .rest
            .get(ACT_CLAIM)
            .and_then(serde_json::Value::as_object)
            .and_then(|act| act.get("sub"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        // Run identity (workbench backend P1): present iff `run_id` is a
        // non-empty string. The other two are optional companions; a token
        // is not invalid for lacking any of them.
        let claim_str = |name: &str| {
            claims
                .rest
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let run = claim_str(RUN_ID_CLAIM).map(|run_id| RunClaims {
            run_id,
            root_event_id: claim_str(ROOT_EVENT_ID_CLAIM),
            trace_id: claim_str(TRACE_ID_CLAIM),
        });

        // A gateway-minted WORKBENCH agent token (P2-6) names the agent as
        // `sub` and the human as `act.sub`, but its authority is the human's:
        // the ACL resolves ownership and directory groups from the subject,
        // so the subject must be the human — otherwise a member could mint
        // `agent:<x>` and inherit whatever that principal owns (codex
        // second-opinion review of P2). The agent stays visible as the actor
        // for attribution. A runner-minted per-run token is not touched: its
        // authority IS the agent's (admin), by design.
        let workbench = claims
            .rest
            .get(DELEGATION_PURPOSE_CLAIM)
            .and_then(serde_json::Value::as_str)
            == Some(WORKBENCH_AGENT_PURPOSE);
        let (subject, actor) = match (workbench, actor) {
            (true, Some(human)) => (human, Some(claims.sub)),
            (_, actor) => (claims.sub, actor),
        };

        // A service role belongs to a service's own credential: a machine token (minted for a run,
        // an agent or a skill, or acting for someone) never carries one, whatever its roles say —
        // so a run token a runner minted cannot claim events or mint further tokens.
        let machine = agent_minted || run.is_some() || actor.is_some();
        let service = if machine {
            None
        } else if has_role_value(
            &claims.rest,
            &self.config.admin_role_claim,
            &self.config.runner_role_value,
        ) {
            Some(ServiceRole::Runner)
        } else if has_role_value(
            &claims.rest,
            &self.config.admin_role_claim,
            &self.config.bridge_role_value,
        ) {
            Some(ServiceRole::Bridge)
        } else {
            None
        };
        let issued_at = claims.rest.get("iat").and_then(serde_json::Value::as_u64);

        let agent_skill = claim_str(SKILL_CLAIM);
        let agent = claims.rest.get(AGENT_CLAIM).map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or_default()
                .to_owned()
        });

        Ok(AuthContext {
            subject,
            tenant_id,
            role,
            operator,
            groups,
            actor,
            run,
            agent_skill,
            agent,
            bridge_turn: false,
            service,
            issued_at,
            asker: None,
        })
    }

    /// Pick the trust entry whose issuer matches the token's `iss`.
    /// The `iss` is read without verifying the signature (routing only).
    /// Single-issuer configs skip the probe — there is one entry and the
    /// verified `set_issuer` check below still pins `iss`.
    fn select_entry(&self, token: &str) -> Result<&TrustEntry, AuthError> {
        if let [only] = self.entries.as_slice() {
            return Ok(only);
        }
        let mut probe = Validation::new(Algorithm::RS256);
        probe.insecure_disable_signature_validation();
        probe.validate_exp = false;
        probe.validate_aud = false;
        probe.required_spec_claims = std::collections::HashSet::new();
        let iss = decode::<IssProbe>(token, &DecodingKey::from_secret(&[]), &probe)
            .map_err(|e| AuthError::Invalid(e.to_string()))?
            .claims
            .iss
            .ok_or_else(|| AuthError::Invalid("token missing `iss` claim".to_owned()))?;
        self.entries
            .iter()
            .find(|e| e.issuer == iss)
            .ok_or_else(|| AuthError::Invalid(format!("untrusted issuer `{iss}`")))
    }

    #[must_use]
    pub fn config(&self) -> &OidcConfig {
        &self.config
    }
}

/// How far the bridge host's clock may lead the gateway's (crew C-F3).
const BRIDGE_CLOCK_SKEW_SECS: u64 = 60;

/// A bridge token's lifetime bound. `exp - iat` is the minter's own claim and
/// is held to [`escurel_types::bridge::MAX_TTL_SECS`] exactly; `exp - now`
/// compares two clocks, so a bridge whose clock leads the gateway's by up to
/// [`BRIDGE_CLOCK_SKEW_SECS`] is not locked out. Expiry itself has no leeway.
fn bridge_lifetime_ok(iat: u64, exp: u64, now: u64) -> bool {
    let max = escurel_types::bridge::MAX_TTL_SECS;
    exp.saturating_sub(iat) <= max && exp.saturating_sub(now) <= max + BRIDGE_CLOCK_SKEW_SECS
}

/// What every bridge TURN token must be, whoever signed it: no agent, skill,
/// run or purpose claim (refused, not ignored: no bridge turn carries one, so
/// one present means a key is being misused), a numeric `iat`/`exp` at most
/// [`escurel_types::bridge::MAX_TTL_SECS`] apart, a valid channel in
/// [`escurel_types::bridge::CHANNEL_CLAIM`], and a tenant. `act` is refused
/// unless `allow_act`. Returns `(iat, captured_via, tenant)`.
fn bridge_turn_shape(
    claims: &Claims,
    tenant_claim: &str,
    allow_act: bool,
) -> Result<(u64, String, String), AuthError> {
    use escurel_types::bridge;
    let refuse = |why: String| Err(AuthError::BridgeRefused(why));
    let forbidden: &[&str] = if allow_act {
        &[
            AGENT_CLAIM,
            SKILL_CLAIM,
            RUN_ID_CLAIM,
            ROOT_EVENT_ID_CLAIM,
            DELEGATION_PURPOSE_CLAIM,
        ]
    } else {
        &[
            AGENT_CLAIM,
            SKILL_CLAIM,
            RUN_ID_CLAIM,
            ROOT_EVENT_ID_CLAIM,
            DELEGATION_PURPOSE_CLAIM,
            ACT_CLAIM,
        ]
    };
    for claim in forbidden {
        if claims.rest.contains_key(*claim) {
            return refuse(format!("a bridge token may not carry `{claim}`"));
        }
    }
    let num = |k: &str| claims.rest.get(k).and_then(serde_json::Value::as_u64);
    let (Some(iat), Some(exp)) = (num("iat"), num("exp")) else {
        return refuse("a bridge token needs numeric `iat` and `exp`".to_owned());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if !bridge_lifetime_ok(iat, exp, now) {
        return refuse(format!(
            "a bridge token lives at most {}s",
            bridge::MAX_TTL_SECS
        ));
    }
    let Some(via) = claims
        .rest
        .get(bridge::CHANNEL_CLAIM)
        .and_then(serde_json::Value::as_str)
        .and_then(bridge::via)
    else {
        return refuse(format!(
            "a bridge token names its channel in `{}`",
            bridge::CHANNEL_CLAIM
        ));
    };
    let tenant_id = claims
        .rest
        .get(tenant_claim)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AuthError::MissingTenant {
            tenant_claim: tenant_claim.to_owned(),
        })?
        .to_owned();
    Ok((iat, via, tenant_id))
}

/// A token a chat bridge signed itself, verified against the bridge trust
/// entry (the self-signed path, deprecated by #830).
///
/// The bridge holds a key that can sign for anyone, so the gateway — not the
/// bridge — bounds what such a token is ([`bridge_turn_shape`]): never admin
/// or operator, and honouring no roles, groups or `act` — the gateway does not
/// know the person's IdP groups here, so group-granted ACLs do not apply. Its
/// actor is `bridge:<channel>`, stamped as `captured_via` on what it captures.
fn bridge_context(
    claims: Claims,
    tenant_claim: &str,
    pinned_tenant: Option<&str>,
) -> Result<AuthContext, AuthError> {
    let (iat, via, tenant_id) = bridge_turn_shape(&claims, tenant_claim, false)?;
    if let Some(pinned) = pinned_tenant
        && tenant_id != pinned
    {
        return Err(AuthError::BridgeRefused(format!(
            "a bridge token names tenant `{tenant_id}`; this bridge serves `{pinned}`"
        )));
    }
    Ok(AuthContext {
        subject: claims.sub,
        tenant_id,
        role: Role::Agent,
        operator: false,
        groups: Vec::new(),
        actor: Some(via),
        run: None,
        agent_skill: None,
        agent: None,
        bridge_turn: true,
        service: None,
        issued_at: Some(iat),
        asker: None,
    })
}

/// A bridge TURN token from an ordinary trust entry: one the gateway minted
/// itself (`mint_user_token`, #830) for the person a bridge answers, or for
/// the group principal a mapped group chat acts as.
///
/// Confined exactly like a self-signed bridge token ([`bridge_turn_shape`]:
/// the bridge gate's few tools, at most 900 s, never admin or operator, no
/// service role), but its groups ARE honoured: the gateway resolved them
/// itself from its configured group directories (Graph, Google, Okta) and
/// never puts a reserved name there — any role value is dropped anyway. Only
/// accepted under [`OidcConfig::minting_identity`]. `act.sub`
/// names the person who asked when the subject is a group principal — audit
/// only, like every actor.
fn minted_bridge_context(claims: Claims, cfg: &OidcConfig) -> Result<AuthContext, AuthError> {
    let (iat, via, tenant_id) = bridge_turn_shape(&claims, &cfg.tenant_claim, true)?;
    let asker = match claims.rest.get(ACT_CLAIM) {
        None => None,
        Some(act) => Some(
            act.get("sub")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    AuthError::BridgeRefused("a bridge token's `act` names a `sub`".to_owned())
                })?,
        ),
    };
    // The gateway never puts a role value here; a token that does is honoured without it, so no
    // deployment role (a custom admin value included) rides a turn into a run (crew S-F6).
    let roles = [
        &cfg.admin_role_value,
        &cfg.operator_role_value,
        &cfg.runner_role_value,
        &cfg.bridge_role_value,
    ];
    let groups = claims
        .rest
        .get(&cfg.groups_claim)
        .map(parse_groups_claim)
        .unwrap_or_default()
        .into_iter()
        .filter(|g| escurel_types::chat_channel::is_assignable_group(g) && !roles.contains(&g))
        .collect();
    Ok(AuthContext {
        subject: claims.sub,
        tenant_id,
        role: Role::Agent,
        operator: false,
        groups,
        actor: Some(via),
        run: None,
        agent_skill: None,
        agent: None,
        bridge_turn: true,
        service: None,
        issued_at: Some(iat),
        asker,
    })
}

fn has_admin_role(claims: &serde_json::Map<String, serde_json::Value>, cfg: &OidcConfig) -> bool {
    has_role_value(claims, &cfg.admin_role_claim, &cfg.admin_role_value)
}

fn has_role_value(
    claims: &serde_json::Map<String, serde_json::Value>,
    claim: &str,
    value: &str,
) -> bool {
    let Some(v) = claims.get(claim) else {
        return false;
    };
    match v {
        serde_json::Value::Array(arr) => arr.iter().any(|item| item.as_str() == Some(value)),
        serde_json::Value::String(s) => s == value,
        _ => false,
    }
}

/// Project the `groups_claim` value into a flat list of group names.
///
/// - a JSON **array of strings** → each element (non-string elements
///   dropped);
/// - a single **string** → split on whitespace **and** commas
///   (Keycloak/Auth0 both occur in the wild), trimmed, empties dropped;
/// - any other shape → empty.
///
/// Reserved-name (`public`/`owner`/`admin`) and admin-value stripping is
/// deliberately NOT done here — the verifier stays dumb; the ACL layer
/// owns that policy.
fn parse_groups_claim(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .collect(),
        serde_json::Value::String(s) => s
            .split([' ', '\t', '\n', '\r', ','])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Algorithms we trust at validate-time. RSA SHA-256/384/512 +
/// ECDSA P-256/P-384. Asymmetric only — no HMAC.
fn allowed_algorithms() -> &'static [Algorithm] {
    &[
        Algorithm::RS256,
        Algorithm::RS384,
        Algorithm::RS512,
        Algorithm::ES256,
        Algorithm::ES384,
    ]
}

#[cfg(test)]
mod bridge_lifetime_tests {
    use super::*;

    /// A bridge whose clock leads the gateway's mints `iat`/`exp` in the
    /// gateway's future (crew C-F3): within the skew it is accepted; the
    /// minter's own `exp - iat` stays bounded exactly.
    #[test]
    fn a_bridge_clock_a_little_ahead_is_not_locked_out() {
        let max = escurel_types::bridge::MAX_TTL_SECS;
        let now = 1_000_000;
        for lead in [0, 1, 30, BRIDGE_CLOCK_SKEW_SECS] {
            let iat = now + lead;
            assert!(bridge_lifetime_ok(iat, iat + max, now), "lead {lead}s");
            assert!(bridge_lifetime_ok(iat, iat + max - 60, now), "lead {lead}s");
        }
        let iat = now + BRIDGE_CLOCK_SKEW_SECS + 1;
        assert!(!bridge_lifetime_ok(iat, iat + max, now), "too far ahead");
        assert!(
            !bridge_lifetime_ok(now, now + max + 1, now),
            "too long a life"
        );
    }
}
