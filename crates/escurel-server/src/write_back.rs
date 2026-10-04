//! Human-gated write-back to the system behind a row (stage 4c).
//!
//! A change to an upstream row is proposed as a DRAFT whose frontmatter carries a reserved
//! `write_back` block; nothing reaches the upstream until a human PROMOTES the draft. The promote
//! hook ([`run`]) then:
//!
//! 1. re-reads the row and refuses (`write_back_conflict`) when it no longer matches the `base_etag`
//!    the proposal was based on, so a change never overwrites what nobody reviewed;
//! 2. records an `applying` audit event BEFORE calling out (audit-first);
//! 3. applies the change with the draft id as the idempotency key and bounded retries;
//! 4. records the outcome (`applied` / `failed` / `rejected`) as a durable event;
//! 5. returns the notes WITHOUT the intent, to be committed and the draft closed by the caller.
//!
//! The `applied` event is the witness that makes a re-promote safe: when the local commit failed
//! after the upstream applied, promoting again skips the upstream, so it is never called twice. An
//! upstream with no idempotency support is called at most once: an `applying` event with no outcome
//! is an unknown outcome and refuses until an operator looks.
//!
//! Audit events carry the endpoint NAME, the column names and hashes; never a URL, a secret or a
//! value.

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, RandomState};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use escurel_index::backend::rows::split_instance_page_id;
use escurel_index::{EventKind, Indexer, NewEvent};
use serde_json::{Map, Value, json};

use crate::egress::Egress;
use crate::remote_backend::{self, WriteFail};
use crate::remote_rows;

/// How many times one promotion tries the upstream before it gives up.
pub(crate) const MAX_ATTEMPTS: u32 = 3;
const AUDIT_LABEL: &str = "escurel:write-back";

/// The reserved `write_back` block of a draft.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Intent {
    /// Frontmatter field name → new value (scalars only).
    pub patch: Map<String, Value>,
    /// The etag of the row as the proposer saw it (`expand`'s `backend_projection.etag`).
    pub base_etag: Option<String>,
}

/// A page's frontmatter as a JSON object (the YAML mapping, converted).
pub(crate) fn frontmatter_json<T: serde::Serialize>(fields: &T) -> Map<String, Value> {
    serde_json::to_value(fields)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The etag of a row's projected fields: a hash of their canonical JSON. What a reviewer saw is
/// what is compared, so an unrelated upstream change to an unprojected column is not a conflict.
#[must_use]
pub(crate) fn etag_of(fields: &Map<String, Value>) -> String {
    let sorted: BTreeMap<&String, &Value> = fields.iter().collect();
    let json = serde_json::to_string(&sorted).unwrap_or_default();
    format!("w1:{}", escurel_index::drafts::content_hash(&json))
}

/// Read the `write_back` block out of a page's frontmatter fields. `Ok(None)` when absent;
/// `Err` when present but malformed (a patch that is not an object of scalars).
pub(crate) fn parse_intent(fields: &Map<String, Value>) -> Result<Option<Intent>, String> {
    let Some(block) = fields.get("write_back") else {
        return Ok(None);
    };
    let obj = block
        .as_object()
        .ok_or_else(|| "`write_back` must be a mapping with a `patch`".to_owned())?;
    let patch = obj
        .get("patch")
        .and_then(Value::as_object)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "`write_back.patch` must be a non-empty mapping".to_owned())?;
    if let Some((k, _)) = patch
        .iter()
        .find(|(_, v)| !matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_)))
    {
        return Err(format!(
            "`write_back.patch.{k}` must be a string, number or boolean"
        ));
    }
    Ok(Some(Intent {
        patch: patch.clone(),
        base_etag: obj
            .get("base_etag")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }))
}

/// The page text without its top-level `write_back:` block (the intent is a one-shot instruction,
/// not page content). Text-level so the author's own formatting survives.
#[must_use]
pub(crate) fn strip_intent(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let (mut in_fm, mut skipping) = (false, false);
    for (i, line) in content.split_inclusive('\n').enumerate() {
        let t = line.trim_end_matches(['\r', '\n']);
        if i == 0 && t == "---" {
            in_fm = true;
        } else if in_fm {
            if t == "---" {
                in_fm = false;
                skipping = false;
            } else if skipping {
                if t.is_empty() || t.starts_with([' ', '\t']) {
                    continue;
                }
                skipping = false;
            }
            if in_fm && t.starts_with("write_back:") {
                skipping = true;
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

fn refusal(code: &str, message: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "issues": [{
            "severity": "error",
            "code": code,
            "location": "write_back",
            "message": message.into(),
        }],
    })
}

/// A short jittered pause: `base * 2^(attempt-1)` plus up to 25 %, so concurrent promotions do not
/// retry in lock-step. The jitter comes from a randomly keyed hasher (not the clock's nanoseconds,
/// which two promotions started in the same instant share).
pub(crate) fn backoff(base: Duration, attempt: u32) -> Duration {
    let scaled = base.saturating_mul(1 << (attempt - 1).min(6));
    let roll = RandomState::new().hash_one((attempt, std::time::Instant::now())) % 1000;
    scaled + scaled / 4 * u32::try_from(roll).unwrap_or(0) / 1000
}

/// One audit event. Idempotent per `event_id`, so a retried promotion rewrites the same row.
#[allow(clippy::too_many_arguments)]
async fn audit(
    state: &crate::server::AppState,
    indexer: &Indexer,
    event_id: &str,
    title: &str,
    target_page_id: &str,
    body: &Value,
) -> Result<(), String> {
    match indexer
        .capture_event(NewEvent {
            event_id: Some(event_id.to_owned()),
            at: Some(escurel_index::now_rfc3339_micros()),
            source: "escurel".to_owned(),
            mime: "application/json".to_owned(),
            label_skill: AUDIT_LABEL.to_owned(),
            instance_page_id: Some(target_page_id.to_owned()),
            title: title.to_owned(),
            body: body.to_string(),
            provenance: Some(json!({ "write_back": body, "captured_by": "escurel" })),
            kind: EventKind::System,
            root_event_id: None,
            run_id: None,
        })
        .await
    {
        Ok(stored) => {
            let _ = state.events_tx.send(std::sync::Arc::new(stored));
            Ok(())
        }
        Err(e) => {
            tracing::warn!(event_id, error = %e, "write-back audit event not recorded");
            Err(e.to_string())
        }
    }
}

/// The audit write that records an upstream call that ALREADY HAPPENED: the call cannot be taken
/// back, so a failure is retried a few times and then logged loudly; the draft's own state stays
/// the source of truth (a re-promote recognises a row that already holds the change).
async fn audit_after_apply(
    state: &crate::server::AppState,
    indexer: &Indexer,
    event_id: &str,
    title: &str,
    target_page_id: &str,
    body: &Value,
) {
    for attempt in 1..=3u32 {
        if audit(state, indexer, event_id, title, target_page_id, body)
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50 * u64::from(attempt))).await;
    }
    tracing::error!(
        event_id,
        "the upstream applied a write-back but its witness could not be recorded"
    );
}

/// One lock per draft: promotions of the same draft run one after another, so two racing promotes
/// cannot both reach the upstream (the second finds the first's witness and completes locally).
fn draft_lock(draft_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Forget locks nobody holds or waits on, so the map does not grow with every draft ever made.
    g.retain(|_, m| Arc::strong_count(m) > 1);
    Arc::clone(g.entry(draft_id.to_owned()).or_default())
}

/// A caller must not be able to file the gateway's own bookkeeping ids.
pub(crate) const RESERVED_EVENT_ID_PREFIX: &str = "write-back:";

/// The audit event under `event_id`, but only if the GATEWAY wrote it: a system event under the
/// reserved label from source `escurel`.
async fn witness(
    indexer: &Indexer,
    event_id: &str,
) -> Result<Option<escurel_index::EventInfo>, String> {
    Ok(indexer
        .get_event(event_id)
        .await
        .map_err(|e| e.to_string())?
        .filter(|e| {
            e.label_skill == AUDIT_LABEL && e.source == "escurel" && e.kind == EventKind::System
        }))
}

/// Whether the recorded `failed` event says the upstream did NOT apply (it refused, or nothing was
/// ever sent): only then may a call without an idempotency key be repeated.
async fn definitely_not_applied(indexer: &Indexer, id_failed: &str) -> bool {
    let Ok(Some(ev)) = witness(indexer, id_failed).await else {
        return false;
    };
    let Ok(body) = serde_json::from_str::<Value>(&ev.body) else {
        return false;
    };
    matches!(body["outcome"].as_str(), Some("rejected" | "conflict"))
        || body["attempts"].as_u64() == Some(0)
}

/// The promote hook. Returns the content to commit as the row's notes (the draft's content without
/// its intent), or the refusal to answer `promote_draft` with. A draft with no intent passes through
/// untouched.
///
/// A promotion that carries an intent is SERIALISED per draft and runs in its own task: the upstream
/// call and the witness that records it must not depend on the request that started them. A client
/// that disconnects, or a proxy that times out, drops the request future; without this the upstream
/// could apply the change and the witness never be written, leaving a draft that conflicts with its
/// own already-applied change forever.
pub(crate) async fn run(
    state: &crate::server::AppState,
    indexer: &Indexer,
    draft_id: &str,
    target_page_id: &str,
    decided_by: &str,
    content: &str,
) -> Result<String, Value> {
    // Cheap passthrough: no intent, nothing to serialise or spawn.
    let Ok(parsed) = escurel_md::parse(content) else {
        return Ok(content.to_owned());
    };
    if matches!(
        parse_intent(&frontmatter_json(&parsed.frontmatter.fields)),
        Ok(None)
    ) {
        return Ok(content.to_owned());
    }
    let Some(owned_indexer) = state
        .indexer
        .as_ref()
        .map(escurel_index::IndexerHandle::current)
    else {
        return run_inner(
            state,
            indexer,
            draft_id,
            target_page_id,
            decided_by,
            content,
        )
        .await;
    };
    let lock = draft_lock(draft_id);
    let state = state.clone();
    let (draft_id, target_page_id, decided_by, content) = (
        draft_id.to_owned(),
        target_page_id.to_owned(),
        decided_by.to_owned(),
        content.to_owned(),
    );
    tokio::spawn(async move {
        let _turn = lock.lock().await;
        run_inner(
            &state,
            &owned_indexer,
            &draft_id,
            &target_page_id,
            &decided_by,
            &content,
        )
        .await
    })
    .await
    .unwrap_or_else(|e| {
        Err(refusal(
            "write_back_failed",
            format!("the write-back task did not complete: {e}"),
        ))
    })
}

async fn run_inner(
    state: &crate::server::AppState,
    indexer: &Indexer,
    draft_id: &str,
    target_page_id: &str,
    decided_by: &str,
    content: &str,
) -> Result<String, Value> {
    let Ok(parsed) = escurel_md::parse(content) else {
        return Ok(content.to_owned());
    };
    let intent = match parse_intent(&frontmatter_json(&parsed.frontmatter.fields)) {
        Ok(None) => return Ok(content.to_owned()),
        Ok(Some(i)) => i,
        Err(e) => return Err(refusal("write_back_invalid", e)),
    };
    let stripped = strip_intent(content);
    let id_applying = format!("write-back:{draft_id}:applying");
    let id_applied = format!("write-back:{draft_id}:applied");
    let id_failed = format!("write-back:{draft_id}:failed");
    // The audit trail is read before anything is sent (the witnesses): when that read fails the store
    // is unhealthy, nothing was sent, and the person is told so in words, not with a SQL error.
    let internal = |e: String| {
        tracing::warn!(draft = draft_id, error = %e, "write-back audit trail unreadable");
        refusal(
            "write_back_failed",
            "the audit trail could not be read, so nothing was sent; try again",
        )
    };

    // The witness: the upstream already applied this draft. Do not call it again.
    if witness(indexer, &id_applied)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Ok(stripped);
    }

    let Some((skill, row_id)) = split_instance_page_id(target_page_id) else {
        return Err(refusal(
            "write_back_unsupported",
            "a write-back targets a row page",
        ));
    };
    let src = remote_rows::source(indexer, skill)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            refusal(
                "write_back_unsupported",
                format!("skill `{skill}` is not a remote `rows` skill; nothing to write back to"),
            )
        })?;
    // The allow-list is enforced HERE as well as at `create_draft`: a promoter's corrected content
    // is a second way in, and it must not widen what the skill declared writable.
    if let Some(f) = intent
        .patch
        .keys()
        .find(|f| !src.cfg.writable_columns.contains(*f))
    {
        return Err(refusal(
            "backend_read_only_field",
            format!(
                "`{f}` is not a writable column of `{skill}` (writable: {:?})",
                src.cfg.writable_columns
            ),
        ));
    }
    let Some(write) = src.remote.write.clone() else {
        return Err(refusal(
            "backend_read_only",
            format!("skill `{skill}` declares no `write` op"),
        ));
    };
    let egress: &Egress = &state.egress;
    let idempotent = matches!(write, escurel_index::RemoteOp::Http { .. })
        || src.remote.write_idempotency_arg.is_some();

    // An `applying` event with no outcome is an upstream call whose result nobody saw. With an
    // idempotency key it is safe to repeat; without one, repeating could apply it twice.
    let in_flight = witness(indexer, &id_applying)
        .await
        .map_err(internal)?
        .is_some();
    // ...unless the recorded outcome says the upstream REFUSED (or nothing was ever sent): then it
    // certainly did not apply, and a human who fixed the cause may promote again.
    if in_flight && !idempotent && !definitely_not_applied(indexer, &id_failed).await {
        return Err(refusal(
            "write_back_unknown_outcome",
            "an earlier attempt may have reached the upstream and its outcome was not recorded; \
             this endpoint has no idempotency support, so it is not repeated. Check the upstream \
             and discard the draft, or reconcile it.",
        ));
    }

    // (1) Re-read and compare: what the reviewer saw must still be what is there.
    let (row, upstream_etag) = match remote_rows::get_with_etag(egress, &src, row_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(refusal(
                "row_not_found",
                format!("`{skill}` has no object `{row_id}` upstream"),
            ));
        }
        Err(e) => {
            // Nothing was sent, but the person promoted a change and must be able to see that it did
            // not go through: record the dead-letter (no attempt was made) before refusing.
            audit_after_apply(
                state,
                indexer,
                &id_failed,
                "write-back-failed",
                target_page_id,
                &json!({
                    "draft_id": draft_id,
                    "endpoint": src.ep.name,
                    "skill": skill,
                    "key": row_id,
                    "columns": intent.patch.keys().collect::<Vec<_>>(),
                    "decided_by": decided_by,
                    "outcome": "failed",
                    "attempts": 0,
                }),
            )
            .await;
            tracing::warn!(skill, row = row_id, draft = draft_id, error = %e, "write-back pre-read failed");
            state.metrics.inc_write_back("failed");
            return Err(refusal(
                "write_back_failed",
                "the source could not be reached to check the row before changing it; nothing was sent",
            ));
        }
    };
    let current = etag_of(&row.fields);
    // The change is ALREADY there: an earlier call reached the upstream but its witness was lost (a
    // crash between the call and the audit write, or a dropped request). The row now carries our
    // own change, so its etag no longer matches the draft's base: that is not a conflict, it is
    // "applied". Record the witness and let the caller finish its local half.
    if intent.patch.iter().all(|(field, want)| {
        row.fields
            .get(field)
            .is_some_and(|have| same_scalar(have, want))
    }) {
        audit_after_apply(
            state,
            indexer,
            &id_applied,
            "write-back-applied",
            target_page_id,
            &json!({
                "draft_id": draft_id,
                "endpoint": src.ep.name,
                "skill": skill,
                "key": row_id,
                "columns": intent.patch.keys().collect::<Vec<_>>(),
                "decided_by": decided_by,
                "outcome": "applied",
                "attempts": 0,
                "note": "the row already held the change; no call was made",
            }),
        )
        .await;
        state.metrics.inc_write_back("applied");
        return Ok(stripped);
    }
    if intent.base_etag.as_deref().is_some_and(|b| b != current) {
        state.metrics.inc_write_back("conflict");
        return Err(refusal(
            "write_back_conflict",
            "the row changed upstream since this change was proposed; re-read it and propose again",
        ));
    }

    // The patch, in the UPSTREAM's own field names.
    let mut payload = Map::new();
    for (field, value) in &intent.patch {
        let Some(path) = src.remote.project.get(field) else {
            return Err(refusal(
                "backend_read_only_field",
                format!("`{field}` is not a projected field"),
            ));
        };
        let Some(upstream_key) = simple_key(path) else {
            return Err(refusal(
                "write_back_unmappable",
                format!("`{field}` maps to a nested path and cannot be written back"),
            ));
        };
        payload.insert(upstream_key, value.clone());
    }
    let columns: Vec<&String> = intent.patch.keys().collect();
    let patch_hash = escurel_index::drafts::content_hash(
        &serde_json::to_string(&intent.patch).unwrap_or_default(),
    );
    let audit_body = |outcome: &str, attempts: u32| {
        json!({
            "draft_id": draft_id,
            "endpoint": src.ep.name,
            "skill": skill,
            "key": row_id,
            "columns": columns,
            "before_etag": current,
            "patch_hash": patch_hash,
            "decided_by": decided_by,
            "outcome": outcome,
            "attempts": attempts,
        })
    };

    // (2) Audit first, and FATAL: an upstream call with no record of intent is the one thing the
    // audit trail exists to prevent, so when the record cannot be written nothing is sent.
    if audit(
        state,
        indexer,
        &id_applying,
        "write-back-applying",
        target_page_id,
        &audit_body("applying", 0),
    )
    .await
    .is_err()
    {
        return Err(refusal(
            "write_back_failed",
            "the audit trail could not be written, so nothing was sent; try again",
        ));
    }

    // (3) Apply, with the draft id as the idempotency key.
    let mut last = String::new();
    let mut rejected = false;
    let mut attempt = 0;
    // Without an idempotency key a repeat could apply twice, so the call is made AT MOST ONCE.
    let max_attempts = if idempotent { MAX_ATTEMPTS } else { 1 };
    while attempt < max_attempts {
        attempt += 1;
        match remote_backend::call_write(
            egress,
            &src.limiter_key,
            &src.ep,
            &src.remote,
            row_id,
            &payload,
            draft_id,
            upstream_etag.as_deref(),
        )
        .await
        {
            Ok(()) => {
                // (4) The durable witness.
                audit_after_apply(
                    state,
                    indexer,
                    &id_applied,
                    "write-back-applied",
                    target_page_id,
                    &audit_body("applied", attempt),
                )
                .await;
                state.metrics.inc_write_back("applied");
                return Ok(stripped);
            }
            Err(WriteFail::Conflict) => {
                audit_after_apply(
                    state,
                    indexer,
                    &id_failed,
                    "write-back-conflict",
                    target_page_id,
                    &audit_body("conflict", attempt),
                )
                .await;
                state.metrics.inc_write_back("conflict");
                return Err(refusal(
                    "write_back_conflict",
                    "the upstream refused the change: the row changed since it was read",
                ));
            }
            Err(WriteFail::Final(m)) => {
                last = m;
                rejected = true;
                break;
            }
            Err(WriteFail::Retryable(m)) => {
                last = m;
                if attempt < max_attempts {
                    tokio::time::sleep(backoff(egress.policy().write_retry_backoff, attempt)).await;
                }
            }
        }
    }
    // Dead-letter: recorded, the draft stays open, and a later promote may try again.
    state.metrics.inc_write_back("dead_letter");
    let outcome = if rejected { "rejected" } else { "failed" };
    audit_after_apply(
        state,
        indexer,
        &id_failed,
        "write-back-failed",
        target_page_id,
        &audit_body(outcome, attempt),
    )
    .await;
    Err(refusal(
        if rejected {
            "write_back_rejected"
        } else {
            "write_back_failed"
        },
        if rejected {
            format!("the upstream rejected the change: {last}")
        } else {
            format!("the upstream could not be reached after {attempt} attempts: {last}")
        },
    ))
}

/// Whether two scalars are the same value as a person would read them (`7` and `"7"` are).
fn same_scalar(a: &Value, b: &Value) -> bool {
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    };
    matches!((text(a), text(b)), (Some(x), Some(y)) if x == y)
}

/// The upstream key for a projection path: `$.key` or a bare `key`. Nested paths are not writable.
fn simple_key(path: &str) -> Option<String> {
    let p = path
        .strip_prefix("$.")
        .or_else(|| path.strip_prefix('$'))
        .unwrap_or(path);
    (!p.is_empty() && !p.contains(['.', '[', ']'])).then(|| p.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(yaml_like: &Value) -> Map<String, Value> {
        yaml_like.as_object().cloned().unwrap()
    }

    #[test]
    fn the_etag_ignores_key_order_and_changes_with_any_value() {
        let a = fields(&json!({ "b": 1, "a": "x" }));
        let b = fields(&json!({ "a": "x", "b": 1 }));
        assert_eq!(etag_of(&a), etag_of(&b));
        assert_ne!(etag_of(&a), etag_of(&fields(&json!({ "a": "y", "b": 1 }))));
        assert!(etag_of(&a).starts_with("w1:"));
    }

    #[test]
    fn an_intent_is_parsed_and_a_malformed_one_is_refused() {
        let f = fields(
            &json!({ "write_back": { "patch": { "tier": "gold" }, "base_etag": "w1:abc" } }),
        );
        let i = parse_intent(&f).unwrap().unwrap();
        assert_eq!(i.patch["tier"], "gold");
        assert_eq!(i.base_etag.as_deref(), Some("w1:abc"));
        assert_eq!(parse_intent(&fields(&json!({ "x": 1 }))).unwrap(), None);
        for bad in [
            json!("no"),
            json!({}),
            json!({ "patch": {} }),
            json!({ "patch": { "t": [1] } }),
        ] {
            assert!(
                parse_intent(&fields(&json!({ "write_back": bad }))).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn strip_intent_removes_only_the_write_back_block() {
        let src = "---\nkind: instance\nid: c-1\nwrite_back:\n  patch: { tier: gold }\n  base_etag: \"w1:x\"\nskill: customer\n---\nbody with write_back: in text\n";
        assert_eq!(
            strip_intent(src),
            "---\nkind: instance\nid: c-1\nskill: customer\n---\nbody with write_back: in text\n"
        );
        // A one-line flow form, and no front matter at all.
        assert_eq!(
            strip_intent("---\nwrite_back: { patch: { a: 1 } }\nid: x\n---\nb\n"),
            "---\nid: x\n---\nb\n"
        );
        assert_eq!(strip_intent("no frontmatter\n"), "no frontmatter\n");
    }

    #[test]
    fn only_a_flat_key_maps_back_to_the_upstream() {
        assert_eq!(
            simple_key("$.account_tier").as_deref(),
            Some("account_tier")
        );
        assert_eq!(simple_key("tier").as_deref(), Some("tier"));
        assert_eq!(simple_key("$.a.b"), None);
        assert_eq!(simple_key("$.items[0]"), None);
        assert_eq!(simple_key("$"), None);
    }

    #[test]
    fn the_backoff_doubles_and_stays_bounded() {
        let base = Duration::from_millis(100);
        let (a, b) = (backoff(base, 1), backoff(base, 2));
        assert!(a >= base && a <= base * 2, "{a:?}");
        assert!(b >= base * 2 && b <= base * 3, "{b:?}");
    }
}
