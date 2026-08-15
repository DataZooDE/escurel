//! The reader's background refresh task (DuckLake program, PR 5).
//!
//! A reader replica serves an [`escurel_index::IndexerHandle`] that was
//! populated once at boot (`adopt_lake` with `current = None`). Without
//! this task the replica would only ever see that one snapshot until the
//! process restarts. [`RefreshTask`] closes that gap: it polls the lake
//! on an interval, and when a newer snapshot has been published,
//! re-adopts it into a fresh in-memory [`Indexer`][escurel_index::Indexer]
//! and hot-swaps it into the handle — no restart, no dropped in-flight
//! request (`IndexerHandle::current` is captured once per request, so an
//! outstanding `Arc<Indexer>` keeps answering against the snapshot it was
//! captured against; see `escurel_index::snapshot::IndexerHandle` docs).
//!
//! This module is deliberately standalone: it does not read
//! `ESCUREL_ROLE` / `ESCUREL_INDEX_BACKEND` or touch `EscurelConfig`.
//! Wiring a `RefreshTask` into the boot path behind a reader-role check
//! is PR 6's job; this PR only builds the mechanism and proves it with
//! no-mock integration tests.
//!
//! Failure policy (hard requirement): a poll or adopt error is logged
//! and the loop keeps running on the OLD `current_snapshot_id` — a
//! reader must never panic, exit, or stop serving because the lake is
//! briefly unreachable or a publish raced the poll. "Refresh failure
//! keeps serving stale" beats "refresh failure takes the reader down".
//!
//! No manual cleanup on swap: [`adopt_lake`] opens the reader-side
//! indexer via `Connection::open_in_memory()` (see
//! `escurel-index/src/snapshot/lake.rs`), so a swapped-out `Indexer`
//! holds no on-disk reader-local file or other external resource that
//! needs releasing — `Arc`'s own refcounting is enough. There is no
//! `Drop` impl on `Indexer` beyond the compiler-derived one.

use std::sync::Arc;
use std::time::Duration;

use escurel_embed::Embedder;
use escurel_index::IndexerHandle;
use escurel_index::snapshot::{LakeConfig, adopt_lake, latest_lake_snapshot_id};
use escurel_storage::LaneStore;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Polls a DuckLake for a newer published snapshot and hot-swaps it into
/// a live [`IndexerHandle`] when found.
///
/// Constructed directly (no config/env parsing here — see the module
/// doc); a future reader boot path (`ESCUREL_ROLE=reader`, PR 6)
/// supplies every field from its own config and calls [`RefreshTask::spawn`].
pub struct RefreshTask {
    handle: IndexerHandle,
    lake_cfg: LakeConfig,
    store: Arc<dyn LaneStore>,
    embedder: Arc<dyn Embedder>,
    tenant: String,
    interval: Duration,
    /// The snapshot id already being served when this task starts —
    /// normally the id `adopt_lake` returned for the boot-time adopt.
    initial_snapshot_id: Option<i64>,
    /// Shared-surface attaches to re-apply to every freshly adopted
    /// indexer — see [`SharedAttaches`]. `None` = plain corpus reader.
    shared_attaches: Option<SharedAttaches>,
}

/// The shared chat/events/CRDT attaches a Postgres-catalog reader wires
/// onto its indexer at boot (`EscurelConfig::build`'s `is_pg_catalog()`
/// branch). A refresh swap builds a brand-new in-memory indexer, so
/// WITHOUT re-applying these the swap silently downgraded the reader:
/// `has_shared_chat`/`has_shared_events`/`has_shared_crdt` all reverted
/// to false and the dispatch gate started answering
/// `unsupported_on_replica` for tools it had been serving fine. Lake-
/// backed append surfaces hit this on the FIRST tick (their own
/// `CREATE TABLE IF NOT EXISTS` at boot advances the lake snapshot, so
/// a re-adopt is immediate); Postgres-backed ones hit it on the first
/// corpus publish after boot.
#[derive(Clone)]
pub struct SharedAttaches {
    pub chat: crate::config::AppendBackend,
    pub events: crate::config::AppendBackend,
    /// `Some(dsn)` re-attaches the shared CRDT op-log/snapshot tables
    /// on the indexer's own connection (`Indexer::attach_crdt_pg`).
    /// The session-actor `DuckdbCrdtBackend` lives on its own
    /// connection outside the swap and needs no re-attach.
    pub crdt_pg_dsn: Option<String>,
}

impl SharedAttaches {
    /// Apply onto a freshly adopted indexer — the same sequence the
    /// reader boot path runs.
    async fn apply(
        &self,
        indexer: &escurel_index::Indexer,
        lake_cfg: &LakeConfig,
    ) -> Result<(), escurel_index::snapshot::SnapshotError> {
        match self.chat {
            crate::config::AppendBackend::Postgres => {
                indexer.attach_chat_pg(&lake_cfg.catalog_dsn).await?;
            }
            crate::config::AppendBackend::DuckLake => {
                indexer.attach_chat_lake(lake_cfg).await?;
            }
        }
        match self.events {
            crate::config::AppendBackend::Postgres => {
                indexer.attach_events_pg(&lake_cfg.catalog_dsn).await?;
            }
            crate::config::AppendBackend::DuckLake => {
                indexer.attach_events_lake(lake_cfg).await?;
            }
        }
        if let Some(dsn) = &self.crdt_pg_dsn {
            indexer.attach_crdt_pg(dsn).await?;
        }
        Ok(())
    }
}

impl RefreshTask {
    /// Build a task. `initial_snapshot_id` MUST be the snapshot id the
    /// handle is already serving (`None` only if the handle was primed
    /// from a lake that had never been published at boot) — the task
    /// never re-adopts a snapshot the handle already carries.
    #[must_use]
    pub fn new(
        handle: IndexerHandle,
        lake_cfg: LakeConfig,
        store: Arc<dyn LaneStore>,
        embedder: Arc<dyn Embedder>,
        tenant: impl Into<String>,
        interval: Duration,
        initial_snapshot_id: Option<i64>,
    ) -> Self {
        Self {
            handle,
            lake_cfg,
            store,
            embedder,
            tenant: tenant.into(),
            interval,
            initial_snapshot_id,
            shared_attaches: None,
        }
    }

    /// Re-apply these shared-surface attaches after every adopt, before
    /// the swap — a Postgres-catalog reader passes the same backends it
    /// attached at boot. An attach failure keeps serving the CURRENT
    /// indexer (with its working attaches) rather than swapping in a
    /// downgraded one.
    #[must_use]
    pub fn with_shared_attaches(mut self, shared: SharedAttaches) -> Self {
        self.shared_attaches = Some(shared);
        self
    }

    /// Spawn the poll/adopt/swap loop on the current Tokio runtime.
    /// Returns a [`RefreshHandle`] the caller uses for graceful
    /// shutdown — mirrors the `oneshot` shutdown-signal + `JoinHandle`
    /// idiom `escurel_server::server::serve` already uses for its
    /// background sweep task.
    #[must_use]
    pub fn spawn(self) -> RefreshHandle {
        let RefreshTask {
            handle,
            lake_cfg,
            store,
            embedder,
            tenant,
            interval,
            initial_snapshot_id,
            shared_attaches,
        } = self;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let join = tokio::spawn(async move {
            let mut current_snapshot_id = initial_snapshot_id;
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        current_snapshot_id = poll_and_adopt(
                            &handle,
                            &lake_cfg,
                            &store,
                            &embedder,
                            &tenant,
                            current_snapshot_id,
                            shared_attaches.as_ref(),
                        )
                        .await;
                    }
                    _ = &mut shutdown_rx => break,
                }
            }
        });
        RefreshHandle {
            shutdown_tx: Some(shutdown_tx),
            join,
        }
    }
}

/// One poll cycle: check the lake, adopt if newer, swap if adopted.
/// Never panics — every error path logs and returns `current` unchanged
/// so the caller keeps serving the snapshot it already has.
async fn poll_and_adopt(
    handle: &IndexerHandle,
    lake_cfg: &LakeConfig,
    store: &Arc<dyn LaneStore>,
    embedder: &Arc<dyn Embedder>,
    tenant: &str,
    current: Option<i64>,
    shared_attaches: Option<&SharedAttaches>,
) -> Option<i64> {
    let latest = match latest_lake_snapshot_id(lake_cfg).await {
        Ok(latest) => latest,
        Err(e) => {
            tracing::warn!(
                target: "escurel",
                tenant,
                error = %e,
                "reader refresh: lake poll failed, keeping serving stale snapshot"
            );
            return current;
        }
    };
    if latest.is_none() || latest == current {
        tracing::debug!(
            target: "escurel",
            tenant,
            current = ?current,
            latest = ?latest,
            "reader refresh: nothing newer to adopt"
        );
        return current;
    }

    match adopt_lake(
        lake_cfg,
        Arc::clone(store),
        Arc::clone(embedder),
        tenant,
        current,
    )
    .await
    {
        Ok(Some(adopted)) => {
            let snapshot_id = adopted.snapshot_id;
            // Re-wire the shared chat/events/CRDT surfaces BEFORE the
            // swap: the adopted indexer is brand-new and carries none of
            // the boot-time attaches (see `SharedAttaches`). On failure,
            // keep serving the current indexer — a stale snapshot with
            // working shared surfaces beats a fresh one that answers
            // `unsupported_on_replica`.
            if let Some(shared) = shared_attaches
                && let Err(e) = shared.apply(&adopted.indexer, lake_cfg).await
            {
                tracing::warn!(
                    target: "escurel",
                    tenant,
                    error = %e,
                    "reader refresh: shared-surface re-attach failed, \
                     keeping serving stale snapshot"
                );
                return current;
            }
            // Swap in the freshly adopted indexer; drop the returned old
            // one. `Arc`'s refcounting means any in-flight request that
            // captured the old `Arc<Indexer>` via `IndexerHandle::current`
            // before this swap keeps it alive and keeps answering against
            // it — no manual drop-guard needed (see module docs).
            let _old = handle.swap(adopted.indexer);
            tracing::info!(
                target: "escurel",
                tenant,
                previous = ?current,
                snapshot_id,
                "reader refresh: adopted a newer snapshot"
            );
            Some(snapshot_id)
        }
        Ok(None) => {
            // The lake advanced between the poll and the adopt's own
            // no-op gate (or was never published) — nothing to do this
            // tick, still serving `current`.
            current
        }
        Err(e) => {
            tracing::warn!(
                target: "escurel",
                tenant,
                error = %e,
                "reader refresh: adopt failed, keeping serving stale snapshot"
            );
            current
        }
    }
}

/// Handle to a spawned [`RefreshTask`]. Signals shutdown and awaits the
/// loop task — same shape as `escurel_server::server::ServerHandle`'s
/// background-task fields.
pub struct RefreshHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl RefreshHandle {
    /// Signal the loop to stop and await it. `Ok(())` on a clean stop;
    /// a cancelled join (e.g. the runtime is shutting down anyway) is
    /// silenced, matching `ServerHandle::shutdown`.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}
