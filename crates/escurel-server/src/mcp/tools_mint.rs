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
fn rfc3339(t: SystemTime) -> String {
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

pub(super) async fn tool_mint_agent_token(
    state: &AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: MintAgentTokenArgs = parse_args(args, "mint_agent_token")?;
    let Some(signer) = state.signer.as_ref() else {
        return Err(JsonRpcError::invalid_params(
            "mint_agent_token: this gateway has no signing identity (set \
             ESCUREL_AUTH_SIGNING_KEY, and ESCUREL_AUTH_SIGNING_KID / _ISSUER as needed)"
                .to_owned(),
        )
        .with_code("unsupported", false));
    };
    if a.skill.trim().is_empty() {
        return Err(JsonRpcError::invalid_params(
            "mint_agent_token: `skill` is required (the agent runs as `agent:<skill>`)".to_owned(),
        ));
    }
    let ttl = a.ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
    if !(MIN_TTL_SECS..=MAX_TTL_SECS).contains(&ttl) {
        return Err(JsonRpcError::invalid_params(format!(
            "mint_agent_token: `ttl_secs` is {MIN_TTL_SECS}..={MAX_TTL_SECS}, got {ttl}"
        )));
    }
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
            caller.is_admin,
            ttl,
            Some(&run),
        )
        .map_err(|e| match e {
            escurel_auth::SignError::UnusableAgentSubject(_) => {
                JsonRpcError::invalid_params(format!("mint_agent_token: {e}"))
            }
            other => JsonRpcError::internal(format!("mint_agent_token: {other}")),
        })?;
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
