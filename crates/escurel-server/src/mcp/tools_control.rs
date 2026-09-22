//! Controls as events (knowledge-workbench backend P2-2): the authorisation
//! branch `capture_event` runs for the `escurel:run-control` label.
//!
//! A human cancels, retries, pauses, resumes or requeues by capturing a
//! control event; the runner's subscriber acts on it. The gateway stays
//! automation-free — it decides only WHO may ask WHAT, and stamps who
//! asked:
//!
//! - `cancel` / `retry` name a run. The caller must be allowed to write the
//!   run's target page (the page its `run-started` names), under the same
//!   `ESCUREL_WRITE_ACL` gate `update_page` applies. An unknown run and a
//!   forbidden one read the same — `event_not_found` — so the label is no
//!   existence oracle for runs on pages the caller may not see.
//! - `pause` / `resume` / `requeue` are tenant-wide and admin-only; a
//!   non-admin reads the same `event_not_found`.
//! - A malformed request (no action, an unknown one, `cancel` without a
//!   run, a body that is not JSON) is a caller mistake: plain
//!   `invalid_params`, no `event_not_found`.
//!
//! The request is stored as bookkeeping (`kind: system`) on the run's
//! target page — never inbox work, hidden from the default listings,
//! present in the run's own record (`list_events{run_id}`) and under the
//! label the runner tails. `provenance.control` is written here from the
//! token: `{action, run_id?, event_id?, reason?, requested_by}`; a
//! caller-supplied block is replaced, never merged.

use escurel_index::{AclCaller, EventListFilter, Indexer};
use serde_json::{Value, json};

use super::JsonRpcError;

/// The label a control request is captured under.
pub(super) const RUN_CONTROL_LABEL: &str = "escurel:run-control";

/// The actions a control event may carry, and who may ask for them.
const RUN_ACTIONS: [&str; 2] = ["cancel", "retry"];
const TENANT_ACTIONS: [&str; 3] = ["pause", "resume", "requeue"];

/// How far back to look for the run's `run-started` in the run's own
/// record (a run has a handful of lifecycle rows plus progress snapshots).
const RUN_SCAN: usize = 128;

/// What `capture_event` stores for an authorised control request.
pub(super) struct RunControl {
    /// The run's target page for `cancel` / `retry`; `None` tenant-wide.
    pub instance_page_id: Option<String>,
    pub run_id: Option<String>,
    pub root_event_id: Option<String>,
    /// The `provenance.control` block, requester stamped.
    pub control: Value,
}

fn mistake(msg: impl std::fmt::Display) -> JsonRpcError {
    JsonRpcError::invalid_params(format!("capture_event: `escurel:run-control`: {msg}"))
}

/// One message for "no such run" and "not yours to control": a run on a
/// page the caller may not write must not be confirmed to exist.
fn denied() -> JsonRpcError {
    JsonRpcError::invalid_params(
        "capture_event: `escurel:run-control`: no such run, or not yours to control".to_owned(),
    )
    .with_code("event_not_found", false)
}

fn text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty())
}

/// Authorise a control request against the caller and the run it names.
pub(super) async fn authorise_run_control(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    body: &str,
) -> Result<RunControl, JsonRpcError> {
    let req: Value = serde_json::from_str(body).map_err(|e| {
        mistake(format!(
            "the body is JSON `{{action, run_id?, reason?}}` ({e})"
        ))
    })?;
    let Some(action) = text(&req["action"]) else {
        return Err(mistake(format!(
            "`action` is one of {} (per run) or {} (tenant-wide, admin)",
            RUN_ACTIONS.join("|"),
            TENANT_ACTIONS.join("|")
        )));
    };
    let mut control = json!({ "action": action, "requested_by": caller.subject });
    if let Some(reason) = text(&req["reason"]) {
        control["reason"] = json!(reason);
    }

    if RUN_ACTIONS.contains(&action) {
        let Some(run_id) = text(&req["run_id"]) else {
            return Err(mistake(format!("`{action}` names a `run_id`")));
        };
        // The run's own record names its target page on `run-started`.
        let page = indexer
            .list_events_filtered_page(
                &EventListFilter {
                    run_id: Some(run_id.to_owned()),
                    include_system: true,
                    ..Default::default()
                },
                true,
                RUN_SCAN,
                None,
            )
            .await
            .map_err(|e| JsonRpcError::internal(format!("capture_event run-control: {e}")))?;
        let Some(started) = page
            .events
            .iter()
            .find(|e| e.label_skill == "escurel:run" && e.title == "run-started")
        else {
            return Err(denied());
        };
        let target = started.instance_page_id.clone().filter(|p| !p.is_empty());
        if !caller.is_admin {
            // A non-admin controls a run through the page it works on: the
            // same write gate `update_page` applies to that page.
            let Some(target) = target.as_deref() else {
                return Err(denied());
            };
            if write_acl != crate::server::WriteAclMode::Off {
                let Some(existing) = indexer.read_page_markdown(target).await.map_err(|e| {
                    JsonRpcError::internal(format!("capture_event run-control: {e}"))
                })?
                else {
                    return Err(denied());
                };
                let allowed = indexer
                    .may_write_page(caller, target, &existing)
                    .await
                    .map_err(|e| {
                        JsonRpcError::internal(format!("capture_event run-control acl: {e}"))
                    })?;
                if !allowed {
                    if write_acl == crate::server::WriteAclMode::Log {
                        tracing::warn!(
                            subject = %caller.subject, run_id, page_id = %target,
                            "write-ACL would refuse this run-control request (log mode) — allowing"
                        );
                    } else {
                        return Err(denied());
                    }
                }
            }
        }
        control["run_id"] = json!(run_id);
        return Ok(RunControl {
            instance_page_id: target,
            run_id: Some(run_id.to_owned()),
            root_event_id: started.root_event_id.clone(),
            control,
        });
    }

    if TENANT_ACTIONS.contains(&action) {
        if !caller.is_admin {
            return Err(denied());
        }
        if action == "requeue" {
            // Requeue names the dead-lettered event to put back.
            let Some(event_id) = text(&req["event_id"]) else {
                return Err(mistake("`requeue` names the dead-lettered `event_id`"));
            };
            control["event_id"] = json!(event_id);
        }
        return Ok(RunControl {
            instance_page_id: None,
            run_id: None,
            root_event_id: None,
            control,
        });
    }

    Err(mistake(format!(
        "unknown action `{action}`; one of {} (per run) or {} (tenant-wide, admin)",
        RUN_ACTIONS.join("|"),
        TENANT_ACTIONS.join("|")
    )))
}
