//! Writing a page's `pages` and `blocks` rows.
//!
//! Two paths materialise a page: markdown (`update_page`) and document
//! overlays (`update_document_overlay`). Both delete-then-insert the same two
//! tables, and each spelled the column lists out for itself.
//!
//! That is worse than ordinary duplication. The `pages` column list was
//! *character-identical* in both, so nothing signalled that the two must agree
//! — and they must: rows written by one path are read by queries that do not
//! know or care which path wrote them. A column added on one side only would
//! not fail to compile, would not fail a test that exercises one path, and
//! would surface as a document-backed instance missing a field that markdown
//! instances have.
//!
//! The `blocks` lists differed by exactly one column: markdown omitted
//! `context`, letting it default to NULL, while documents wrote it. That is a
//! difference in spelling, not in intent, so the shared helper takes
//! `context: Option<&str>` and markdown passes `None` — same NULL, one list.
//!
//! R6 of `docs/notes/complexity-reduction-plan.md`.

use duckdb::{Transaction, params};

use crate::indexer::BLOCKS_DENSE_VEC_DIM;

/// Replace the `pages` row for `page_id`.
///
/// DELETE + INSERT rather than `ON CONFLICT`: the upsert clause's behaviour
/// varies by DuckDB version, and this keeps the semantics obvious.
#[allow(clippy::too_many_arguments)]
pub(crate) fn upsert_page_row(
    tx: &Transaction<'_>,
    page_id: &str,
    slug: Option<&str>,
    skill: &str,
    page_type: &str,
    frontmatter_json: &str,
    body_hash: &str,
    at_ts: Option<&str>,
    scenario: Option<&str>,
) -> Result<(), duckdb::Error> {
    tx.execute("DELETE FROM pages WHERE page_id = ?", params![page_id])?;
    tx.execute(
        "INSERT INTO pages \
         (page_id, slug, skill, page_type, frontmatter, body_hash, at_ts, scenario, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?::JSON, ?, \
                 TRY_CAST(? AS TIMESTAMP), ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        params![
            page_id,
            slug,
            skill,
            page_type,
            frontmatter_json,
            body_hash,
            at_ts,
            scenario,
        ],
    )?;
    Ok(())
}

/// One `blocks` row, before its dense vector is formatted into SQL.
pub(crate) struct BlockRow<'a> {
    pub anchor: String,
    pub ordinal: i64,
    pub body: &'a str,
    /// The structural situating prefix (GH #216), concatenated only at
    /// embed/FTS/rerank time. `None` writes NULL, which is what the markdown
    /// path has always done by omitting the column.
    pub context: Option<&'a str>,
    /// The embedding, as a checked vector literal. Not a `String`: see
    /// [`DenseVecLiteral`].
    pub dense_vec: DenseVecLiteral,
}

/// A DuckDB vector literal, constructible only from an embedding.
///
/// The dense vector is interpolated into SQL rather than bound — DuckDB's
/// parameter binding has no array type that round-trips to `FLOAT[N]` — so
/// the field it lands in is the one place in this module where a caller could
/// introduce injection. It was a plain `String` on a `pub(crate)` struct,
/// which is not a bug today (every caller passes `format_vector_literal`
/// output) but makes future misuse a typo away (codex review).
///
/// A newtype with a private field and one constructor removes the option:
/// the only way to obtain one is from a `&[f32]`, and floats cannot carry
/// SQL.
pub(crate) struct DenseVecLiteral(String);

impl DenseVecLiteral {
    pub(crate) fn from_embedding(v: &[f32]) -> Self {
        Self(crate::indexer::format_vector_literal(v))
    }
}

/// Replace every `blocks` row for `page_id`.
///
/// The dense vector is interpolated rather than bound (see
/// [`DenseVecLiteral`] for why, and for what keeps that safe).
pub(crate) fn replace_blocks(
    tx: &Transaction<'_>,
    page_id: &str,
    skill: &str,
    page_type: &str,
    at_ts: Option<&str>,
    scenario: Option<&str>,
    blocks: &[BlockRow<'_>],
) -> Result<(), duckdb::Error> {
    tx.execute("DELETE FROM blocks WHERE page_id = ?", params![page_id])?;
    for b in blocks {
        let block_id = format!("{page_id}:{}", b.anchor);
        let sql = format!(
            "INSERT INTO blocks \
             (block_id, page_id, anchor, ordinal, body, context, dense_vec, skill, page_type, at_ts, scenario) \
             VALUES (?, ?, ?, ?, ?, ?, {}::FLOAT[{BLOCKS_DENSE_VEC_DIM}], ?, ?, TRY_CAST(? AS TIMESTAMP), ?)",
            b.dense_vec.0
        );
        tx.execute(
            &sql,
            params![
                block_id, page_id, b.anchor, b.ordinal, b.body, b.context, skill, page_type, at_ts,
                scenario,
            ],
        )?;
    }
    Ok(())
}
