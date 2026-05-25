//! Two-stage reconciler for external markdown edits.
//!
//! When a markdown file on disk diverges from the latest CRDT
//! snapshot for the same `page_id` — typically because a human or an
//! external tool wrote the file outside the Loro session — the
//! reconciler decides which side wins, per
//! `docs/spec/storage.md §CRDT persistence`:
//!
//! * **Stage 1 — cited instances.** Another page references this
//!   one as a wikilink destination (`links.dst_page = page_id`).
//!   The snapshot is canonical; the on-disk content is stale and
//!   will be overwritten the next time the canonical state is
//!   exported. → [`Decision::SnapshotWins`].
//!
//! * **Stage 2 — uncited and brand-new pages.** Nothing links into
//!   the page. The external edit wins: drop any in-memory CRDT
//!   state, import the on-disk markdown into a fresh `LoroDoc`,
//!   write a fresh snapshot row. → [`Decision::ExternalWins`].
//!
//! ## Boundary discipline
//!
//! Citation lookup is a separate trait — [`CitationLookup`] —
//! because the answer lives in `escurel-index`'s `links` table, and
//! pulling `escurel-index` into `escurel-crdt`'s prod deps would
//! couple two crates that otherwise sit side by side. Wiring crates
//! (escurel-server / escurel-cli) supply the production
//! `IndexerCitationLookup` impl; tests in this crate supply a
//! stub.
//!
//! The reconciler itself reads markdown bytes through
//! [`escurel_storage::LaneStore`] and persists snapshots through
//! [`crate::CrdtBackend`], so callers don't have to thread DuckDB
//! handles through the call site.

use std::sync::Arc;

use async_trait::async_trait;
use loro::{ExportMode, LoroDoc};

use escurel_storage::{Key, LaneStore, StoreError};

use crate::{CrdtBackend, Error, Snapshot};

/// Outcome of [`ExternalEditReconciler::reconcile`].
///
/// Returned as a value (not as a side-effect channel) so callers can
/// log / report which branch was taken without re-deriving it from
/// snapshot row counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The snapshot stayed canonical; the on-disk file (if any) is
    /// stale and will be overwritten by the next canonical export.
    /// Returned for cited pages whose on-disk content has drifted.
    SnapshotWins,

    /// The on-disk markdown is now canonical. Either a fresh
    /// snapshot was written (new page, or uncited page with a real
    /// external edit), or no work was needed (on-disk content
    /// already matched the snapshot).
    ExternalWins,
}

/// "Does any other page in this tenant cite `(tenant, page_id)`?"
///
/// True iff at least one row in `links` has `dst_page = page_id`
/// for the tenant. Implementations live in the wiring crate that
/// owns the indexer.
#[async_trait]
pub trait CitationLookup: Send + Sync + 'static {
    /// Return `true` if at least one page links INTO
    /// `(tenant, page_id)`.
    ///
    /// # Errors
    ///
    /// Surfaces whatever the underlying lookup raises (DuckDB query
    /// failure, tenant lookup failure, …). The reconciler treats
    /// any error as fatal for this call — it does not silently fall
    /// back to "uncited", because that would let a query failure
    /// promote stale markdown into the canonical snapshot.
    async fn is_cited(&self, tenant: &str, page_id: &str) -> Result<bool, anyhow::Error>;
}

/// External-edit reconciler.
///
/// Construct once per tenant (or once per process — it's stateless)
/// and call [`reconcile`](Self::reconcile) at page-open time or from
/// a background scan that found a stale snapshot.
pub struct ExternalEditReconciler {
    backend: Arc<dyn CrdtBackend>,
    store: Arc<dyn LaneStore>,
    citations: Arc<dyn CitationLookup>,
}

impl ExternalEditReconciler {
    /// Build a reconciler bound to a backend, lane store, and
    /// citation-lookup impl.
    #[must_use]
    pub fn new(
        backend: Arc<dyn CrdtBackend>,
        store: Arc<dyn LaneStore>,
        citations: Arc<dyn CitationLookup>,
    ) -> Self {
        Self {
            backend,
            store,
            citations,
        }
    }

    /// Reconcile the canonical CRDT state for `page_id` against the
    /// markdown bytes at `md_key`. Returns the branch taken (see
    /// [`Decision`]).
    ///
    /// # Errors
    ///
    /// * [`Error::Duckdb`] if loading the snapshot or writing a new
    ///   one fails.
    /// * [`Error::Loro`] if the snapshot blob or the freshly-imported
    ///   markdown bytes cannot be round-tripped through the engine.
    /// * [`Error::Citation`] if the [`CitationLookup`] errors.
    /// * [`Error::Storage`] if reading the on-disk markdown fails
    ///   for any reason other than "not found" (which is treated as
    ///   "no external content" — a snapshot-only page).
    pub async fn reconcile(
        &self,
        tenant: &str,
        page_id: &str,
        md_key: &Key,
    ) -> Result<Decision, Error> {
        // 1) Try to read the on-disk markdown. Missing-file is not
        //    an error here: a tenant can have CRDT state for a page
        //    whose canonical markdown hasn't been published yet (or
        //    has been deleted). In that case there is no "external
        //    edit" to apply, so the snapshot stays canonical.
        let on_disk: Option<Vec<u8>> = match self.store.read(md_key).await {
            Ok(bytes) => Some(bytes.to_vec()),
            Err(StoreError::NotFound(_)) => None,
            Err(e) => return Err(Error::Storage(e.to_string())),
        };

        // 2) Load the latest snapshot bytes (if any). We ignore the
        //    op tail here — the reconciler operates on the
        //    snapshot-level view, not the live engine. Live sessions
        //    must close before reconciling; running both at once is
        //    out of scope for v1.
        let snapshot_bytes: Option<Vec<u8>> =
            self.backend.load(page_id).await?.and_then(|(snap, _ops)| {
                let bytes = snap.as_bytes().to_owned();
                if bytes.is_empty() {
                    // `load` returns an empty Snapshot when the page
                    // has ops but no snapshot row yet (legacy
                    // pre-snapshot state). Treat that as
                    // "no snapshot" — the live engine owns it, we
                    // don't.
                    None
                } else {
                    Some(bytes)
                }
            });

        match (on_disk, snapshot_bytes) {
            // 3a) Never-snapshotted page with on-disk content:
            //     import the markdown into a fresh LoroDoc, take a
            //     snapshot. Citation status is irrelevant — there's
            //     no snapshot to defend.
            (Some(disk), None) => {
                self.snapshot_from_external(page_id, &disk).await?;
                Ok(Decision::ExternalWins)
            }

            // 3b) Snapshot exists; on-disk markdown also exists.
            //     Compare bodies. Equal → no-op ExternalWins. Diff
            //     → branch on citation status.
            (Some(disk), Some(snap)) => {
                let snap_body = body_from_snapshot(&snap)?;
                if snap_body.as_bytes() == disk.as_slice() {
                    return Ok(Decision::ExternalWins);
                }
                if self
                    .citations
                    .is_cited(tenant, page_id)
                    .await
                    .map_err(|e| Error::Citation(e.to_string()))?
                {
                    Ok(Decision::SnapshotWins)
                } else {
                    self.snapshot_from_external(page_id, &disk).await?;
                    Ok(Decision::ExternalWins)
                }
            }

            // 3c) Snapshot exists; no on-disk content. Treat as a
            //     non-edit — snapshot stays canonical.
            (None, Some(_)) => Ok(Decision::SnapshotWins),

            // 3d) Neither snapshot nor on-disk content. Nothing to
            //     reconcile; report ExternalWins so a caller polling
            //     "did the external state become canonical?" doesn't
            //     have to treat this as a special "absent" case —
            //     the empty external state IS canonical by default.
            (None, None) => Ok(Decision::ExternalWins),
        }
    }

    /// Import the external markdown bytes into a fresh `LoroDoc`,
    /// export a snapshot, and persist it under the next monotonic
    /// hlc for this page. Used for stage-2 wins and for never-
    /// snapshotted pages.
    async fn snapshot_from_external(&self, page_id: &str, disk: &[u8]) -> Result<(), Error> {
        let body = std::str::from_utf8(disk).map_err(|e| Error::Loro(format!("utf-8: {e}")))?;

        let doc = LoroDoc::new();
        doc.get_text("body").insert(0, body)?;
        doc.commit();
        let bytes = doc.export(ExportMode::Snapshot)?;

        // The next snapshot must have a strictly greater hlc than
        // any persisted op or snapshot for this page, otherwise the
        // `(page_id, snapshot_hlc)` PK collides on re-reconcile or
        // a previously-snapshotted page with the same body.
        let next_hlc = self.backend.max_hlc(page_id).await?.saturating_add(1);
        self.backend
            .snapshot(page_id, next_hlc, &Snapshot::new(bytes))
            .await?;
        Ok(())
    }
}

/// Decode a Loro snapshot blob and return its `"body"` text. Used to
/// compare snapshot content against on-disk markdown without keeping
/// the document around.
fn body_from_snapshot(snap: &[u8]) -> Result<String, Error> {
    let doc = LoroDoc::new();
    doc.import(snap)?;
    Ok(doc.get_text("body").to_string())
}
