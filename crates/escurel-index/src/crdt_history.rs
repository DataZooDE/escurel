//! Historical state via CRDT snapshots (M7).
//!
//! An instance's state over time is the projection of its event
//! sequence. We record that history as real Loro **snapshots** in
//! `crdt_snapshots` (server-authorable, unlike peer-anchored ops), and
//! `expand(as_of = T)` materializes the snapshot at-or-before T — the
//! frontmatter+body *as it was* at that instant.
//!
//! [`Indexer::seed_snapshot_history`] writes a page's snapshot timeline;
//! [`load_snapshot_at`] + [`materialize_snapshot`] are the read side
//! used by [`crate::Indexer::expand`].

use duckdb::params;
use escurel_md::wikilink::parse_wikilinks;

use crate::indexer::{Indexer, IndexerError};
use crate::read::{BlockInfo, ExpandedPage, PageRef};

impl Indexer {
    /// Seed a page's CRDT snapshot history: one snapshot per `(taken_at,
    /// markdown)` state, stamped at the given wall-clock time. This is
    /// how the demo gives an instance a *real* state-over-time history
    /// (each snapshot is a genuine Loro export); `expand(as_of=T)` then
    /// replays it. `taken_at` is RFC 3339; `markdown` is the full page
    /// (frontmatter + body) as it was at that instant.
    pub async fn seed_snapshot_history(
        &self,
        page_id: &str,
        states: &[(&str, &str)],
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        // Strictly-increasing hlc above any existing snapshot for the
        // page so the `(page_id, snapshot_hlc)` PK never collides.
        let mut hlc: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(snapshot_hlc), 0) FROM crdt_snapshots WHERE page_id = ?",
                params![page_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        for (taken_at, markdown) in states {
            let bytes = escurel_crdt::snapshot_bytes_from_markdown(markdown)?;
            hlc += 1;
            conn.execute(
                "INSERT INTO crdt_snapshots (page_id, snapshot_hlc, snapshot_bytes, taken_at) \
                 VALUES (?, ?, ?, TRY_CAST(? AS TIMESTAMP))",
                params![page_id, hlc, bytes, taken_at],
            )?;
        }
        Ok(())
    }
}

/// The newest snapshot for `page_id` taken at-or-before `as_of`, or
/// `None` when the page has no snapshot history reaching back that far
/// (the caller then falls through to the current-state path).
pub(crate) fn load_snapshot_at(
    conn: &duckdb::Connection,
    page_id: &str,
    as_of: &str,
) -> Result<Option<Vec<u8>>, IndexerError> {
    match conn.query_row(
        "SELECT snapshot_bytes FROM crdt_snapshots \
         WHERE page_id = ? AND taken_at <= TRY_CAST(? AS TIMESTAMP) \
         ORDER BY taken_at DESC, snapshot_hlc DESC LIMIT 1",
        params![page_id, as_of],
        |r| r.get::<_, Vec<u8>>(0),
    ) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Decode a Loro snapshot blob and re-parse it into an [`ExpandedPage`]
/// — the instance materialized at that historical point.
pub(crate) fn materialize_snapshot(
    page_id: &str,
    snapshot: &[u8],
) -> Result<ExpandedPage, IndexerError> {
    let markdown = escurel_crdt::body_from_snapshot(snapshot)?;
    let parsed = escurel_md::parse(&markdown)?;
    let fields = &parsed.frontmatter.fields;

    let frontmatter = serde_json::to_value(fields)?;
    let skill = fields
        .get("skill")
        .and_then(escurel_md::YamlValue::as_str)
        .or_else(|| fields.get("id").and_then(escurel_md::YamlValue::as_str))
        .unwrap_or("")
        .to_owned();
    let slug = fields
        .get("id")
        .and_then(escurel_md::YamlValue::as_str)
        .map(str::to_owned);
    let page_type = parsed.frontmatter.page_type;

    let body = parsed.body.to_owned();
    let wikilinks_out = parse_wikilinks(&body);
    let blocks = vec![BlockInfo {
        anchor: "blk-0".to_owned(),
        content: body.clone(),
    }];

    Ok(ExpandedPage {
        page: PageRef {
            page_id: page_id.to_owned(),
            slug,
            skill,
            page_type,
        },
        frontmatter,
        body,
        blocks,
        wikilinks_out,
    })
}
