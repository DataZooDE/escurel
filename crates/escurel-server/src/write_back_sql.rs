//! The promote hook for a row of a DATABASE-backed `rows` skill (SQLite / Postgres / MySQL): the
//! counterpart of the REST/MCP arm in [`crate::write_back`], with the same choreography and the same
//! witness/audit events, and a different apply step ([`escurel_index::Indexer::rows_apply_patch`]).
//!
//! 1. The draft's patch may only touch the skill's `writable_columns` (checked again here: a
//!    promoter's corrected content is a second way in).
//! 2. The row is re-read; a row that ALREADY holds the change counts as applied (a crash between the
//!    commit and its witness, or a dropped request); a changed etag is a conflict and nothing is sent.
//! 3. An `applying` audit event is written first and is fatal when it cannot be: no record, no UPDATE.
//! 4. The UPDATE runs in one transaction with the optimistic check built in
//!    ([`escurel_index::Indexer::rows_apply_patch`]); transient failures retry with backoff, then the
//!    promote dead-letters (the draft stays open, a later promote may try again).
//! 5. The outcome is recorded as a durable event, the witness that makes a re-promote safe.

use escurel_index::Indexer;
use escurel_index::backend::{RowWriteError, RowsSource};
use serde_json::{Value, json};

use crate::write_back::{
    Intent, MAX_ATTEMPTS, audit, audit_after_apply, backoff, etag_of, refusal, same_scalar,
};

/// What the generic hook already knows.
pub(crate) struct Request<'a> {
    pub draft_id: &'a str,
    pub target_page_id: &'a str,
    pub decided_by: &'a str,
    pub skill: &'a str,
    pub row_id: &'a str,
    pub intent: &'a Intent,
    /// The draft's content without its intent: what is committed as the row's notes.
    pub stripped: String,
}

pub(crate) async fn run(
    state: &crate::server::AppState,
    indexer: &Indexer,
    req: Request<'_>,
    src: RowsSource,
) -> Result<String, Value> {
    let Request {
        draft_id,
        target_page_id,
        decided_by,
        skill,
        row_id,
        intent,
        stripped,
    } = req;
    let id_applying = format!("write-back:{draft_id}:applying");
    let id_applied = format!("write-back:{draft_id}:applied");
    let id_failed = format!("write-back:{draft_id}:failed");
    let source = src.sql.attach.clone().unwrap_or_default();

    // The allow-list, enforced HERE as well as at `create_draft`.
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
    if !Indexer::rows_source_is_writable(&src) {
        return Err(refusal(
            "backend_read_only",
            format!(
                "skill `{skill}` reads a `{}` source, which cannot be written back",
                src.sql.connector.as_str()
            ),
        ));
    }

    // (1) Re-read and compare.
    let row = match indexer.rows_get(&src, row_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Err(refusal(
                "row_not_found",
                format!("`{skill}` has no row `{row_id}` in its source"),
            ));
        }
        Err(e) => {
            // Nothing was sent, but the person promoted a change and must be able to see it did not
            // go through: record the dead-letter (no attempt was made) before refusing.
            audit_after_apply(
                state,
                indexer,
                &id_failed,
                "write-back-failed",
                target_page_id,
                &json!({
                    "draft_id": draft_id, "source": source, "skill": skill, "key": row_id,
                    "columns": intent.patch.keys().collect::<Vec<_>>(),
                    "decided_by": decided_by, "outcome": "failed", "attempts": 0,
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
    // The change is ALREADY there: an earlier UPDATE committed but its witness was lost. The row now
    // carries our own change, so its etag no longer matches the draft's base: that is "applied".
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
                "draft_id": draft_id, "source": source, "skill": skill, "key": row_id,
                "columns": intent.patch.keys().collect::<Vec<_>>(),
                "decided_by": decided_by, "outcome": "applied", "attempts": 0,
                "note": "the row already held the change; no update was made",
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
            "the row changed in the source since this change was proposed; re-read it and propose again",
        ));
    }

    let columns: Vec<&String> = intent.patch.keys().collect();
    let patch_hash = escurel_index::drafts::content_hash(
        &serde_json::to_string(&intent.patch).unwrap_or_default(),
    );
    let audit_body = |outcome: &str, attempts: u32| {
        json!({
            "draft_id": draft_id, "source": source, "skill": skill, "key": row_id,
            "columns": columns, "before_etag": current, "patch_hash": patch_hash,
            "decided_by": decided_by, "outcome": outcome, "attempts": attempts,
        })
    };

    // (2) Audit first, and FATAL.
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

    // (3) Apply: one transaction, the optimistic check inside it.
    let mut last = String::new();
    let mut rejected = false;
    let mut attempt = 0;
    while attempt < MAX_ATTEMPTS {
        attempt += 1;
        match indexer
            .rows_apply_patch(&src, row_id, &row, &intent.patch)
            .await
        {
            Ok(()) => {
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
            Err(RowWriteError::Conflict) => {
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
                    "the source refused the change: the row changed since it was read",
                ));
            }
            Err(RowWriteError::NotWritable(m) | RowWriteError::Final(m)) => {
                last = m;
                rejected = true;
                break;
            }
            Err(RowWriteError::Transient(m)) => {
                last = m;
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(backoff(state.egress.policy().write_retry_backoff, attempt))
                        .await;
                }
            }
        }
    }
    // Dead-letter: recorded, the draft stays open, and a later promote may try again.
    state.metrics.inc_write_back("dead_letter");
    audit_after_apply(
        state,
        indexer,
        &id_failed,
        "write-back-failed",
        target_page_id,
        &audit_body(if rejected { "rejected" } else { "failed" }, attempt),
    )
    .await;
    tracing::warn!(skill, row = row_id, draft = draft_id, error = %last, "sql write-back did not apply");
    Err(refusal(
        if rejected {
            "write_back_rejected"
        } else {
            "write_back_failed"
        },
        if rejected {
            format!("the source rejected the change: {last}")
        } else {
            format!("the source could not be written after {attempt} attempts: {last}")
        },
    ))
}
