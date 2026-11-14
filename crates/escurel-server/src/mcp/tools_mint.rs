//! `mint_agent_token` (knowledge-workbench backend P2-6 — BRD FR-M-3): the
//! gateway hands an interactive agent a run-bound bearer.
//!
//! The runner mints its own per-run tokens; a workbench session has no
//! runner, so the gateway mints instead — with its own signing identity
//! (`ESCUREL_AUTH_SIGNING_*`, a key some trusted issuer's JWKS publishes).
//! The token names the agent (`agent:<skill>`), keeps the human visible as
//! the actor (`act.sub`), carries the CALLER's own authority and never more
//! (an admin's mint is admin, a member's mint is their groups), and the run
//! identity claims the rest of the backend keys on: drafts made with it are
//! stamped, `report_progress` accepts it, `list_events{run_id}` shows it.
//!
//! The gateway also plays the runner's part in the run's record: it writes
//! `run-started` at mint (harness `workbench`) and, when the token lapses
//! without a terminal, `run-finished { status: "expired" }`. Which runs are
//! still open is derived from the events themselves (hardening H4:
//! `run-started` rows minted by the gateway whose `expires_at` passed with
//! no `run-finished`), so a restart forgets nothing. `run:<id>:finished` is
//! first-writer-wins, so a run the agent finished explicitly is not
//! overwritten by the sweep.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use escurel_index::{AclCaller, EventKind, Indexer, NewEvent};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{JsonRpcError, parse_args};
use crate::server::AppState;

const DEFAULT_TTL_SECS: u64 = 30 * 60;
const MIN_TTL_SECS: u64 = 1;
const MAX_TTL_SECS: u64 = 4 * 60 * 60;

#[derive(Deserialize)]
pub(super) struct MintAgentTokenArgs {
    #[serde(default)]
    skill: String,
    #[serde(default)]
    root_event_id: Option<String>,
    #[serde(default)]
    target_page_id: Option<String>,
    #[serde(default)]
    ttl_secs: Option<u64>,
    #[serde(default)]
    trace_id: Option<String>,
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a `SystemTime`, without a date crate (the
/// gateway carries none): Howard Hinnant's days-to-civil algorithm.
pub(super) fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// The policy every gateway mint shares (`mint_agent_token`, `mint_run_token`,
/// `issue_agent_token`): who may mint at all, with which signing identity, and
/// for how long. One place, so a new mint tool cannot forget a refusal.
pub(super) struct MintGuard<'a> {
    tool: &'static str,
    pub(super) signer: &'a escurel_auth::Signer,
}

impl<'a> MintGuard<'a> {
    /// Refuse a machine caller, a gateway without a signing identity, and a
    /// restricted caller whose child would shed the restriction.
    pub(super) fn check(
        tool: &'static str,
        state: &'a AppState,
        caller: &AclCaller<'_>,
    ) -> Result<Self, JsonRpcError> {
        // A machine caller must not mint: the child carries the caller's groups but none of its
        // agent or skill claims, so an agent-profile or narrowed token would shed its `scopes`, its
        // skill confinement and the `/ws` / `/ingest` / `/blob` refusals in one call.
        if caller.agent.is_some() || super::tools_write::is_machine_caller(caller) {
            return Err(JsonRpcError {
                code: -32001,
                message: format!(
                    "{tool}: an agent or run token cannot mint another token (a person's or \
                     service's own credential mints)"
                ),
                data: None,
            }
            .with_code("agent_scope_denied", false));
        }
        let Some(signer) = state.signer.as_deref() else {
            return Err(JsonRpcError::invalid_params(format!(
                "{tool}: this gateway has no signing identity (set ESCUREL_AUTH_SIGNING_KEY, and \
                 ESCUREL_AUTH_SIGNING_KID / _ISSUER as needed)"
            ))
            .with_code("unsupported", false));
        };
        // A restricted caller's child inherits the restriction through its groups, which the
        // minter writes to `roles`. A gateway that reads groups from another claim would see a
        // child without them, i.e. unrestricted: refuse rather than mint that.
        if state.role_tools.restriction(caller.token_groups).is_some()
            && state
                .verifier
                .as_ref()
                .is_some_and(|v| v.config().groups_claim != "roles")
        {
            return Err(JsonRpcError::invalid_params(format!(
                "{tool}: a restricted role cannot mint while ESCUREL_AUTH_GROUPS_CLAIM is not \
                 `roles` (the child would lose the restriction)"
            ))
            .with_code("unsupported", false));
        }
        Ok(Self { tool, signer })
    }

    /// The token's lifetime: `requested`, else `default`, refused outside `bounds`.
    pub(super) fn ttl(
        &self,
        requested: Option<u64>,
        default: u64,
        bounds: std::ops::RangeInclusive<u64>,
    ) -> Result<u64, JsonRpcError> {
        let ttl = requested.unwrap_or(default);
        if bounds.contains(&ttl) {
            Ok(ttl)
        } else {
            Err(JsonRpcError::invalid_params(format!(
                "{}: `ttl_secs` is {}..={}, got {ttl}",
                self.tool,
                bounds.start(),
                bounds.end()
            )))
        }
    }

    /// A signing failure as the caller sees it: an unusable subject is theirs to fix.
    pub(super) fn sign_error(&self, e: escurel_auth::SignError) -> JsonRpcError {
        match e {
            escurel_auth::SignError::UnusableAgentSubject(_) => {
                JsonRpcError::invalid_params(format!("{}: {e}", self.tool))
            }
            other => JsonRpcError::internal(format!("{}: {other}", self.tool)),
        }
    }
}

pub(super) async fn tool_mint_agent_token(
    state: &AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: MintAgentTokenArgs = parse_args(args, "mint_agent_token")?;
    let guard = MintGuard::check("mint_agent_token", state, &caller)?;
    if a.skill.trim().is_empty() {
        return Err(JsonRpcError::invalid_params(
            "mint_agent_token: `skill` is required (the agent runs as `agent:<skill>`)".to_owned(),
        ));
    }
    let ttl = guard.ttl(a.ttl_secs, DEFAULT_TTL_SECS, MIN_TTL_SECS..=MAX_TTL_SECS)?;
    let signer = guard.signer;
    let text = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
    let root_event_id = text(&a.root_event_id);
    let target_page_id = text(&a.target_page_id);
    let trace_id = text(&a.trace_id);

    let run_id = ulid::Ulid::new().to_string();
    let run = escurel_auth::MintRunClaims {
        run_id: run_id.clone(),
        // A workbench session with no trigger event is its own root.
        root_event_id: root_event_id.clone().unwrap_or_else(|| run_id.clone()),
        trace_id: trace_id.clone(),
    };
    let token = signer
        .mint_workbench_agent(
            caller.subject,
            &a.skill,
            caller.token_groups,
            // NEVER admin, whoever minted it: the token is an AGENT's. It carries the minter's groups
            // (what the person may read and write) and names them as `act.sub` for the audit trail,
            // but an agent steered by injected text must not hold the admin tools or the exemptions an
            // admin has (it is the one caller the autonomy gate exists for). An admin who wants admin
            // authority uses their own token.
            false,
            ttl,
            Some(&run),
        )
        .map_err(|e| guard.sign_error(e))?;
    let expires_at = SystemTime::now() + Duration::from_secs(ttl);
    let expires = rfc3339(expires_at);
    let subject = format!("agent:{}", a.skill);

    // The gateway plays the runner's part: the run exists from here.
    let mut runner = json!({
        "run_id": run_id,
        "root_event_id": run.root_event_id,
        "harness": "workbench",
        "minted_by": "gateway",
        "requested_by": caller.subject,
        "agent": subject,
        "expires_at": expires,
        "max_attempts": 1,
    });
    if let Some(t) = &trace_id {
        runner["trace_id"] = json!(t);
    }
    let started = indexer
        .capture_event(NewEvent {
            event_id: Some(format!("run:{run_id}:started")),
            at: Some(rfc3339(SystemTime::now())),
            source: "escurel-gateway".to_owned(),
            mime: "application/json".to_owned(),
            label_skill: "escurel:run".to_owned(),
            instance_page_id: target_page_id.clone(),
            title: "run-started".to_owned(),
            body: json!({ "harness": "workbench", "expires_at": expires, "ttl_secs": ttl })
                .to_string(),
            provenance: Some(json!({ "runner": runner })),
            kind: EventKind::System,
            root_event_id: Some(run.root_event_id.clone()),
            run_id: Some(run_id.clone()),
        })
        .await;
    match started {
        Ok(stored) => {
            let _ = state.events_tx.send(Arc::new(stored));
        }
        Err(e) => tracing::warn!(error = %e, run_id, "mint_agent_token: run-started not written"),
    }
    tracing::info!(subject = %caller.subject, agent = %subject, run_id, ttl, "mint_agent_token: minted");
    Ok(json!({
        "token": token,
        "run_id": run_id,
        "root_event_id": run.root_event_id,
        "subject": subject,
        "expires_at": expires,
    }))
}

/// `mint_run_token`'s lifetime: a run's own budget, as the runner floors it.
const RUN_DEFAULT_TTL_SECS: u64 = 30 * 60;
const RUN_MIN_TTL_SECS: u64 = 60;
const RUN_MAX_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Deserialize)]
pub(super) struct MintRunTokenArgs {
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    root_event_id: Option<String>,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    skill: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    grant: Option<String>,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

/// A run id is an id (the runner's ULIDs), never a path or a claim separator.
fn is_run_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether `caller` may have a token for run `run_id` of `skill`. A run
/// another identity started is that identity's (minting for it would let one
/// runner act as another's run and file into its lineage). A runner credential
/// must also have started the run itself, as a run of `skill`: without that
/// binding it could name any skill and collect that skill's write groups.
async fn check_run_binding(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    run_id: &str,
    skill: &str,
) -> Result<(), JsonRpcError> {
    const TOOL: &str = "mint_run_token";
    let started = indexer
        .get_event(&format!("run:{run_id}:started"))
        .await
        .map_err(|e| JsonRpcError::internal(format!("{TOOL}: {e}")))?;
    let refuse = |code: &'static str, why: String| {
        Err(JsonRpcError::forbidden(format!("{TOOL}: run `{run_id}` {why}")).with_code(code, false))
    };
    match started {
        Some(started)
            if escurel_index::captured_by(&started.provenance) != Some(caller.subject) =>
        {
            refuse(
                "run_not_yours",
                "was started by another identity".to_owned(),
            )
        }
        // An admin keeps the rule it always had: any run not left to another identity.
        _ if caller.is_admin => Ok(()),
        None => refuse(
            "run_unknown",
            "has no `run-started` from this caller (file it before asking for the run's token)"
                .to_owned(),
        ),
        Some(started) if started.provenance["runner"]["skill"].as_str() != Some(skill) => {
            refuse("run_not_yours", format!("is not a run of `{skill}`"))
        }
        Some(_) => Ok(()),
    }
}

/// `mint_run_token` (#828): a runner with no signing key of its own gets each
/// run's token here — exactly what a minting runner signs for itself
/// (`escurel-runner-core`'s `TokenSource`): an agent-profile token, or one
/// narrowed to the skill's write groups, or (an admin asking for it) the wide
/// admin shape, all naming the CALLER as `act.sub`.
///
/// The token carries the skill's write groups, read from the skill page — so
/// this is where a keyless runner's authority is bounded. A runner credential
/// gets a token only for a run it started itself (`run:<id>:started`, which
/// the runner files before it packages the run) of that very skill, and only
/// as an agent that lists the skill; a skill no page declares is refused
/// rather than minted groupless. A runner is never given the wide shape. An
/// admin may mint for any run another identity has not started. Writes no
/// `run-started`: the runner files its own lifecycle.
pub(super) async fn tool_mint_run_token(
    state: &AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    const TOOL: &str = "mint_run_token";
    let a: MintRunTokenArgs = parse_args(args, TOOL)?;
    let guard = MintGuard::check(TOOL, state, &caller)?;
    // The dispatch admin gate lets admins and runners in; this is the handler's own word on it.
    if !caller.may_keep_books() {
        return Err(JsonRpcError::forbidden(format!(
            "{TOOL}: only a runner's own credential (escurel:runner or admin) mints run tokens"
        )));
    }
    if !is_run_id(&a.run_id) {
        return Err(JsonRpcError::invalid_params(format!(
            "{TOOL}: `run_id` is 1..=128 characters of [A-Za-z0-9._-]"
        )));
    }
    if a.skill.trim().is_empty() {
        return Err(JsonRpcError::invalid_params(format!(
            "{TOOL}: `skill` is required (the run's label skill)"
        )));
    }
    let wide = match a.grant.as_deref() {
        None | Some("") | Some("narrowed") => false,
        Some("wide") => true,
        Some(other) => {
            return Err(JsonRpcError::invalid_params(format!(
                "{TOOL}: `grant` is `narrowed` or `wide`, got `{other}`"
            )));
        }
    };
    let ttl = guard.ttl(
        a.ttl_secs,
        RUN_DEFAULT_TTL_SECS,
        RUN_MIN_TTL_SECS..=RUN_MAX_TTL_SECS,
    )?;
    check_run_binding(indexer, &caller, &a.run_id, &a.skill).await?;
    let Some(skill) = indexer
        .skill(&a.skill)
        .await
        .map_err(|e| JsonRpcError::internal(format!("{TOOL}: {e}")))?
    else {
        return Err(JsonRpcError::invalid_params(format!(
            "{TOOL}: no skill page declares `{}` in this tenant",
            a.skill
        ))
        .with_code("skill_unknown", false));
    };
    let text = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
    let run = escurel_auth::MintRunClaims {
        run_id: a.run_id.clone(),
        root_event_id: text(&a.root_event_id).unwrap_or_else(|| a.run_id.clone()),
        trace_id: text(&a.trace_id),
    };
    let groups = escurel_index::write_groups(skill.acl.as_ref());
    let signer = guard.signer;
    let (token, subject) = match text(&a.agent) {
        Some(agent) => {
            let Some(profile) = indexer
                .agent_profile(&agent)
                .await
                .map_err(|e| JsonRpcError::internal(format!("{TOOL}: {e}")))?
            else {
                return Err(JsonRpcError::invalid_params(format!(
                    "{TOOL}: no usable agent page `{agent}` in this tenant"
                ))
                .with_code("agent_unknown", false));
            };
            // The agent page bounds which skills run as the agent: a token for a skill it does not
            // list would carry that skill's groups under the agent's name.
            if !profile.lists_skill(&a.skill) {
                return Err(JsonRpcError {
                    code: -32001,
                    message: format!(
                        "{TOOL}: agent `{agent}` does not list the skill `{}`",
                        a.skill
                    ),
                    data: None,
                }
                .with_code("agent_scope_denied", false));
            }
            let token = signer
                .mint_agent_profile(caller.subject, &agent, &a.skill, ttl, Some(&run), &groups)
                .map_err(|e| guard.sign_error(e))?;
            (token, format!("agent:{agent}"))
        }
        None => {
            // The wide shape is the admin authority under the run's subject: an admin's to ask
            // for, never a runner credential's.
            let token = if wide && caller.is_admin {
                signer.mint_agent(caller.subject, &a.skill, ttl, Some(&run))
            } else {
                signer.mint_agent_narrowed(caller.subject, &a.skill, ttl, Some(&run), &groups)
            }
            .map_err(|e| guard.sign_error(e))?;
            (token, format!("agent:{}", a.skill))
        }
    };
    let expires = rfc3339(SystemTime::now() + Duration::from_secs(ttl));
    tracing::info!(subject = %caller.subject, agent = %subject, run_id = %a.run_id, ttl, "mint_run_token: minted");
    Ok(json!({
        "token": token,
        "run_id": run.run_id,
        "root_event_id": run.root_event_id,
        "subject": subject,
        "expires_at": expires,
    }))
}

/// `issue_agent_token`'s lifetime: long enough for a configured harness, bounded
/// so a forgotten one lapses; revocation is the `token_epoch`, not the expiry.
const AGENT_DEFAULT_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const AGENT_MIN_TTL_SECS: u64 = 60 * 60;
const AGENT_MAX_TTL_SECS: u64 = 90 * 24 * 60 * 60;

#[derive(Deserialize)]
pub(super) struct IssueAgentTokenArgs {
    #[serde(default)]
    agent: String,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

/// `issue_agent_token` (#829): an admin hands an EXTERNAL agent (Claude Code,
/// Codex, Multica) a revocable bearer that runs as the agent profile `agent`:
/// `sub: agent:<id>`, the admin as `act.sub`, `escurel:agent` plus the write
/// groups of the skills the agent lists, and the `agent` claim — so its
/// `scopes.tools` / `scopes.write` confine it at dispatch exactly as they do a
/// run, and `/ws`, `/ingest` and `/blob` refuse it. No run claims, no skill.
/// Revoked by the agent page's `token_epoch`. The token is returned once and
/// never logged.
pub(super) async fn tool_issue_agent_token(
    state: &AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    const TOOL: &str = "issue_agent_token";
    let a: IssueAgentTokenArgs = parse_args(args, TOOL)?;
    let guard = MintGuard::check(TOOL, state, &caller)?;
    if !caller.is_admin {
        return Err(JsonRpcError::forbidden(format!(
            "{TOOL}: only an admin issues agent tokens"
        )));
    }
    let ttl = guard.ttl(
        a.ttl_secs,
        AGENT_DEFAULT_TTL_SECS,
        AGENT_MIN_TTL_SECS..=AGENT_MAX_TTL_SECS,
    )?;
    let agent = a.agent.trim();
    let profile = indexer
        .agent_profile(agent)
        .await
        .map_err(|e| JsonRpcError::internal(format!("{TOOL}: {e}")))?
        .ok_or_else(|| {
            JsonRpcError::invalid_params(format!(
                "{TOOL}: no usable agent page `{agent}` in this tenant (missing or archived)"
            ))
            .with_code("agent_unknown", false)
        })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // An epoch past the next second is a malformed page (it would revoke every token ever
    // issued, this one too): say so rather than hand out a token that is refused at once.
    if profile.token_epoch.is_some_and(|epoch| epoch > now + 1) {
        return Err(JsonRpcError::invalid_params(format!(
            "{TOOL}: agent `{agent}` has a `token_epoch` in the future; fix the page (it is Unix \
             seconds)"
        ))
        .with_code("agent_token_epoch_invalid", false));
    }
    let mut groups: Vec<String> = Vec::new();
    for skill in &profile.skills {
        for g in indexer
            .skill_write_groups(skill)
            .await
            .map_err(|e| JsonRpcError::internal(format!("{TOOL}: {e}")))?
        {
            if !groups.contains(&g) {
                groups.push(g);
            }
        }
    }
    let token = guard
        .signer
        .mint_external_agent(caller.subject, agent, ttl, &groups, profile.token_epoch)
        .map_err(|e| guard.sign_error(e))?;
    let expires = rfc3339(SystemTime::now() + Duration::from_secs(ttl));
    // The token itself is never logged: who issued what, for how long, is.
    tracing::info!(subject = %caller.subject, agent = %agent, ttl, "issue_agent_token: issued");
    Ok(json!({
        "token": token,
        "agent": agent,
        "subject": format!("agent:{agent}"),
        "expires_at": expires,
    }))
}

/// Close every gateway-minted run whose token has lapsed with no terminal:
/// `run-finished { status: "expired" }`, first-writer-wins on the id. The
/// set is read from the events (H4), so it survives a restart.
pub(crate) async fn sweep_expired_minted_runs(state: &AppState) {
    let Some(indexer) = state
        .indexer
        .as_ref()
        .map(escurel_index::IndexerHandle::current)
    else {
        return;
    };
    let now = SystemTime::now();
    let due = match indexer.expired_gateway_runs(&rfc3339(now)).await {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!(error = %e, "mint_agent_token: expiry sweep could not read the runs");
            return;
        }
    };
    for run in due {
        let finished = indexer
            .capture_event(NewEvent {
                event_id: Some(format!("run:{}:finished", run.run_id)),
                at: Some(rfc3339(now)),
                source: "escurel-gateway".to_owned(),
                mime: "application/json".to_owned(),
                label_skill: "escurel:run".to_owned(),
                instance_page_id: run.instance_page_id.clone(),
                title: "run-finished".to_owned(),
                body: json!({
                    "status": "expired",
                    "attempts": 0,
                    "held": false,
                    "summary": "the agent token lapsed without a terminal",
                    "reason": "expired",
                })
                .to_string(),
                provenance: Some(json!({ "runner": {
                    "run_id": run.run_id,
                    "root_event_id": run.root_event_id,
                    "harness": "workbench",
                    "minted_by": "gateway",
                    "attempt": 0,
                } })),
                kind: EventKind::System,
                root_event_id: run.root_event_id.clone(),
                run_id: Some(run.run_id.clone()),
            })
            .await;
        match finished {
            Ok(stored) => {
                let _ = state.events_tx.send(Arc::new(stored));
                tracing::info!(run_id = %run.run_id, "mint_agent_token: run expired");
            }
            Err(e) => {
                tracing::warn!(error = %e, run_id = %run.run_id, "mint_agent_token: expiry not written")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_the_epoch_and_a_known_instant() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // 2026-09-22T10:00:00Z
        let t = UNIX_EPOCH + Duration::from_secs(1_790_071_200);
        assert_eq!(rfc3339(t), "2026-09-22T10:00:00Z");
    }
}
