//! Distributed runtimes (#818): claim leases on inbox events, and the
//! runtime directory.
//!
//! * `claim_event` / `renew_claim` — which runner takes an event. A lease row
//!   in the gateway-local [`EventClaims`](crate::event_claims::EventClaims)
//!   table, held by the caller's `sub` (qualified by `#<holder_id>` so
//!   replicas sharing one credential still exclude each other). Machine-only:
//!   the runner's own admin identity may claim, a run token or a person's
//!   token may not — a person who could claim could park any event out of
//!   every runner's reach.
//! * `list_runtimes` — the `runtime` instances the caller may read, each
//!   joined with its runner's `runner-status:<runner_id>` record, so a reader
//!   gets online / stale in one call. Liveness is derived here from the
//!   heartbeat the runner already writes; the page is not rewritten per
//!   heartbeat (lake snapshot churn).

use std::time::{Duration, UNIX_EPOCH};

use escurel_index::{AclCaller, Indexer};
use escurel_types::runtime::{RUNTIME_SKILL, RuntimeHarness, RuntimeInfo};
use serde::Deserialize;
use serde_json::{Value, json};

use super::tools_write::is_machine_caller;
use super::{JsonRpcError, parse_args};
use crate::event_claims::{ClaimOutcome, EventClaims, Lease, now_ms};

/// The default lease when a claim names none.
pub(crate) const DEFAULT_LEASE_SECS: u64 = 60;
/// A lease longer than an hour is a parked event, not a run in progress.
pub(crate) const MAX_CLAIM_LEASE_SECS: u64 = 3600;
/// A runtime is stale once silent for this many reporting intervals.
const STALE_INTERVALS: u64 = 3;

#[derive(Deserialize)]
pub(super) struct ClaimArgs {
    event_id: String,
    #[serde(default)]
    lease_secs: Option<u64>,
    #[serde(default)]
    holder_id: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) enum ClaimVerb {
    Claim,
    Renew,
}

impl ClaimVerb {
    fn tool(self) -> &'static str {
        match self {
            Self::Claim => "claim_event",
            Self::Renew => "renew_claim",
        }
    }
}

/// RFC 3339 for Unix milliseconds.
fn rfc3339_ms(ms: i64) -> String {
    super::tools_mint::rfc3339(UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0)))
}

fn lease_json(lease: &Lease) -> (Value, Value) {
    (json!(lease.holder), json!(rfc3339_ms(lease.expires_ms)))
}

pub(super) async fn tool_claim(
    claims: &EventClaims,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    verb: ClaimVerb,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let tool = verb.tool();
    let a: ClaimArgs = parse_args(args, tool)?;
    if !caller.is_admin || is_machine_caller(&caller) {
        return Err(JsonRpcError::invalid_params(format!(
            "{tool}: only a runner's own admin identity claims events (not a run token, not a \
             person's token)"
        ))
        .with_code("forbidden", false));
    }
    if a.event_id.trim().is_empty() {
        return Err(JsonRpcError::invalid_params(format!(
            "{tool}: `event_id` is required"
        )));
    }
    let lease_secs = a.lease_secs.unwrap_or(DEFAULT_LEASE_SECS);
    if !(1..=MAX_CLAIM_LEASE_SECS).contains(&lease_secs) {
        return Err(JsonRpcError::invalid_params(format!(
            "{tool}: `lease_secs` is 1..={MAX_CLAIM_LEASE_SECS}, got {lease_secs}"
        )));
    }
    let holder = match a
        .holder_id
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        None => caller.subject.to_owned(),
        Some(r)
            if r.len() <= 128
                && r.chars().all(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '@')
                }) =>
        {
            format!("{}#{r}", caller.subject)
        }
        Some(_) => {
            return Err(JsonRpcError::invalid_params(format!(
                "{tool}: `holder_id` is 1..=128 characters of [A-Za-z0-9._:@-]"
            )));
        }
    };
    let event = indexer
        .get_event(&a.event_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("{tool}: {e}")))?
        .ok_or_else(|| {
            JsonRpcError::invalid_params(format!("{tool}: no event `{}`", a.event_id))
                .with_code("event_not_found", false)
        })?;
    // A processed event is done: claiming it would only let a second runner
    // whose inbox read was a moment stale run it again.
    if event.status == "processed" {
        return Ok(json!({
            "event_id": a.event_id,
            "claimed": false,
            "holder": null,
            "expires_at": null,
            "processed": true,
        }));
    }
    let lease_ms = i64::try_from(lease_secs * 1000).unwrap_or(i64::MAX);
    let now = now_ms();
    let outcome: ClaimOutcome = match verb {
        ClaimVerb::Claim => {
            // The capture position only matters while the boot grace is on.
            let seq = if claims.in_grace(now) {
                indexer
                    .event_seq(&a.event_id)
                    .await
                    .map_err(|e| JsonRpcError::internal(format!("{tool}: {e}")))?
            } else {
                None
            };
            claims.claim(&a.event_id, &holder, lease_ms, now, seq)
        }
        ClaimVerb::Renew => claims.renew(&a.event_id, &holder, lease_ms, now),
    }
    .map_err(|e| JsonRpcError::internal(format!("{tool}: {e}")))?;
    let (holder, expires_at) = lease_json(&outcome.lease);
    let won = match verb {
        ClaimVerb::Claim => "claimed",
        ClaimVerb::Renew => "renewed",
    };
    Ok(json!({
        "event_id": a.event_id,
        won: outcome.won,
        "holder": holder,
        "expires_at": expires_at,
        "processed": false,
    }))
}

/// The live claim on `event_id` as the `list_events{event_id}` lookup shows
/// it, or `None`.
pub(super) fn claim_view(claims: &EventClaims, event_id: &str) -> Option<Value> {
    let lease = claims.live(event_id, now_ms()).ok().flatten()?;
    let (holder, expires_at) = lease_json(&lease);
    Some(json!({ "holder": holder, "expires_at": expires_at }))
}

pub(super) async fn tool_list_runtimes(
    indexer: &Indexer,
    caller: AclCaller<'_>,
) -> Result<Value, JsonRpcError> {
    let instances = indexer
        .list_instances(RUNTIME_SKILL, None, None, None, None, None)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_runtimes: {e}")))?;
    let now = now_ms();
    let mut runtimes = Vec::with_capacity(instances.len());
    for i in instances {
        if !indexer
            .may_read_instance(&caller, &i.skill, &i.frontmatter)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_runtimes acl: {e}")))?
        {
            continue;
        }
        let fm = &i.frontmatter;
        let Some(runner_id) = fm["runner_id"].as_str().map(str::to_owned) else {
            continue;
        };
        let status = indexer
            .get_event(&format!("runner-status:{runner_id}"))
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_runtimes: {e}")))?;
        let mut info = RuntimeInfo {
            runner_id,
            page_id: i.page_id.clone(),
            harnesses: serde_json::from_value::<Vec<RuntimeHarness>>(fm["harnesses"].clone())
                .unwrap_or_default(),
            labels: serde_json::from_value(fm["labels"].clone()).unwrap_or_default(),
            max_concurrent: fm["max_concurrent"].as_u64(),
            claim_holder: fm["claim_holder"].as_str().map(str::to_owned),
            status: "unknown".to_owned(),
            ..RuntimeInfo::default()
        };
        if let Some(record) = status {
            let body: Value = serde_json::from_str(&record.body).unwrap_or_default();
            let interval = body["status_interval_s"].as_u64().unwrap_or(30).max(1);
            let stale_after = interval * STALE_INTERVALS;
            let age = record
                .at
                .as_deref()
                .and_then(|at| escurel_index::parse_rfc3339_utc(at, "at").ok())
                .map(|at| u64::try_from((now - at.timestamp_millis()).max(0) / 1000).unwrap_or(0));
            info.status = if record.title == "stopping" {
                "stopped"
            } else if age.is_some_and(|a| a <= stale_after) {
                "online"
            } else {
                "stale"
            }
            .to_owned();
            info.last_seen = record.at.clone();
            info.age_s = age;
            info.stale_after_s = Some(stale_after);
            info.version = body["version"].as_str().map(str::to_owned);
            info.live_runs = body["live_runs"].as_array().map_or(0, |r| r.len() as u64);
        }
        runtimes.push(info);
    }
    runtimes.sort_by(|a, b| a.runner_id.cmp(&b.runner_id));
    Ok(json!({ "runtimes": runtimes }))
}
