//! Production [`CitationLookup`] impl backed by the `links` table.
//!
//! [`CitationLookup`]: escurel_crdt::reconciler::CitationLookup
//!
//! The trait itself lives in `escurel-crdt` so the reconciler
//! doesn't have to import this crate. This module bridges the two:
//! given an [`Indexer`] (which already owns the per-tenant DuckDB
//! connection), it answers "is at least one row in `links` pointing
//! at `dst_page = page_id`?" with a single `SELECT 1 ... LIMIT 1`.
//!
//! ## Tenant argument is ignored — by design
//!
//! Each tenant has its own DuckDB file (and therefore its own
//! [`Indexer`] instance), so the `tenant` parameter on
//! [`CitationLookup::is_cited`] is redundant for this impl. We keep
//! the parameter in the trait so a future multi-tenant indexer (one
//! connection serving N tenants) can use it without breaking the
//! API. Today it is asserted only to surface obvious wiring bugs
//! (passing a different tenant string to the wrong indexer).
//!
//! [`CitationLookup::is_cited`]: escurel_crdt::reconciler::CitationLookup::is_cited

use std::sync::Arc;

use async_trait::async_trait;
use duckdb::params;
use escurel_crdt::reconciler::CitationLookup;

use crate::Indexer;

/// [`CitationLookup`] backed by an [`Indexer`]'s DuckDB connection.
///
/// Construct with [`Self::new`] and pass as
/// `Arc<dyn CitationLookup>` to
/// [`escurel_crdt::ExternalEditReconciler::new`].
pub struct IndexerCitationLookup {
    indexer: Arc<Indexer>,
}

impl IndexerCitationLookup {
    /// Build an [`IndexerCitationLookup`] over an existing
    /// per-tenant [`Indexer`].
    #[must_use]
    pub fn new(indexer: Arc<Indexer>) -> Self {
        Self { indexer }
    }
}

#[async_trait]
impl CitationLookup for IndexerCitationLookup {
    async fn is_cited(&self, _tenant: &str, page_id: &str) -> Result<bool, anyhow::Error> {
        // `EXISTS` would be the textbook choice, but DuckDB's
        // `query_row` with `LIMIT 1` is just as cheap (the
        // `links_dst_skill` index gives us an O(log N) seek) and
        // matches the row-shape contract of `query_row` without
        // forcing a scalar cast on the result.
        //
        // Only `QueryReturnedNoRows` legitimately means "uncited";
        // every other error (missing/corrupt table, connection
        // lost, schema drift) must propagate. Swallowing them
        // with `.ok()` would make the reconciler treat a DuckDB
        // failure as "uncited" and overwrite cited snapshots at
        // the very moment the index is broken (codex review).
        let conn = self.indexer.conn.lock().await;
        match conn.query_row(
            "SELECT 1 FROM links WHERE dst_page = ? LIMIT 1",
            params![page_id],
            |row| row.get::<_, i32>(0),
        ) {
            Ok(_) => Ok(true),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(anyhow::Error::from(e).context("citation lookup failed")),
        }
    }
}
