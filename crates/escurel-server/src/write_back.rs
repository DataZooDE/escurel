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

use std::collections::BTreeMap;
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
/// retry in lock-step.
fn backoff(base: Duration, attempt: u32) -> Duration {
    let scaled = base.saturating_mul(1 << (attempt - 1).min(6));
    let jitter_ns = u64::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos()),
    ) % 1000;
    scaled + scaled / 4 * u32::try_from(jitter_ns).unwrap_or(0) / 1000
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
) {
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
        }
        Err(e) => tracing::warn!(event_id, error = %e, "write-back audit event not recorded"),
    }
}

/// The promote hook. Returns the content to commit as the row's notes (the draft's content without
/// its intent), or the refusal to answer `promote_draft` with. A draft with no intent passes through
/// untouched.
pub(crate) async fn run(
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
    let internal = |e: String| refusal("write_back_failed", e);

    // The witness: the upstream already applied this draft. Do not call it again.
    if indexer
        .get_event(&id_applied)
        .await
        .map_err(|e| internal(e.to_string()))?
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
    let in_flight = indexer
        .get_event(&id_applying)
        .await
        .map_err(|e| internal(e.to_string()))?
        .is_some();
    if in_flight && !idempotent {
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
            audit(
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
            return Err(refusal(
                "write_back_failed",
                "the source could not be reached to check the row before changing it; nothing was sent",
            ));
        }
    };
    let current = etag_of(&row.fields);
    if intent.base_etag.as_deref().is_some_and(|b| b != current) {
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

    // (2) Audit first.
    audit(
        state,
        indexer,
        &id_applying,
        "write-back-applying",
        target_page_id,
        &audit_body("applying", 0),
    )
    .await;

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
                audit(
                    state,
                    indexer,
                    &id_applied,
                    "write-back-applied",
                    target_page_id,
                    &audit_body("applied", attempt),
                )
                .await;
                return Ok(stripped);
            }
            Err(WriteFail::Conflict) => {
                audit(
                    state,
                    indexer,
                    &id_failed,
                    "write-back-conflict",
                    target_page_id,
                    &audit_body("conflict", attempt),
                )
                .await;
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
    let outcome = if rejected { "rejected" } else { "failed" };
    audit(
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
