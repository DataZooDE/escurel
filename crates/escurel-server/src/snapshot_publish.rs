//! The writer's optional periodic publish task (DuckLake program, PR 7).
//!
//! A ducklake writer normally publishes on demand via the
//! `publish_snapshot` admin MCP tool (`crate::mcp::tool_publish_snapshot`).
//! [`PublishTask`] is the opt-in alternative for a deployment that wants
//! the lake to stay current without an operator (or a cron) calling the
//! tool: it ticks on an interval and calls
//! [`escurel_index::snapshot::publish_lake`] every time, trusting
//! `publish_lake`'s OWN internal dirty-check (compare
//! [`escurel_index::Indexer::mutation_epoch`] against the last published
//! epoch, PR 3) to make a clean tick a cheap no-op rather than
//! duplicating that comparison here.
//!
//! Shares `last_published_epoch` with the admin tool (both are handed the
//! SAME `Arc<Mutex<Option<u64>>>` from [`crate::server::AppState`]), so a
//! manual `publish_snapshot` call and a periodic tick never race each
//! other into publishing the same epoch twice.
//!
//! On a successful (non-skipped) publish, runs the retention GC pass
//! ([`escurel_index::snapshot::gc_lake_snapshots`]) as a follow-up step —
//! same "publish, then prune" order the admin tool uses. A GC failure is
//! logged and does not fail the tick (the publish already committed).
//!
//! Same shutdown idiom as [`crate::snapshot_refresh::RefreshTask`]: a
//! `oneshot` shutdown signal + `JoinHandle`, held by the caller
//! ([`EscurelConfig::build`](crate::EscurelConfig::build)) for the
//! process lifetime and shut down alongside every other background task
//! on `SIGTERM`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use escurel_index::IndexerHandle;
use escurel_index::snapshot::{LakeConfig, gc_lake_snapshots, publish_lake};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Periodically publishes the writer's current index state to a
/// DuckLake, then prunes old snapshots down to a retention target.
pub struct PublishTask {
    handle: IndexerHandle,
    lake_cfg: LakeConfig,
    interval: Duration,
    keep: u32,
    last_published_epoch: Arc<Mutex<Option<u64>>>,
}

impl PublishTask {
    /// Build a task. `last_published_epoch` SHOULD be the same
    /// `Arc<Mutex<Option<u64>>>` the `publish_snapshot` admin tool reads
    /// from `AppState`, so the two publish paths stay in sync.
    #[must_use]
    pub fn new(
        handle: IndexerHandle,
        lake_cfg: LakeConfig,
        interval: Duration,
        keep: u32,
        last_published_epoch: Arc<Mutex<Option<u64>>>,
    ) -> Self {
        Self {
            handle,
            lake_cfg,
            interval,
            keep,
            last_published_epoch,
        }
    }

    /// Spawn the publish/GC loop on the current Tokio runtime. Returns a
    /// [`PublishHandle`] for graceful shutdown.
    #[must_use]
    pub fn spawn(self) -> PublishHandle {
        let PublishTask {
            handle,
            lake_cfg,
            interval,
            keep,
            last_published_epoch,
        } = self;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let join = tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        publish_and_gc(&handle, &lake_cfg, keep, &last_published_epoch).await;
                    }
                    _ = &mut shutdown_rx => break,
                }
            }
        });
        PublishHandle {
            shutdown_tx: Some(shutdown_tx),
            join,
        }
    }
}

/// One publish/GC cycle. Never panics: a publish or GC failure is logged
/// and the loop keeps running — a periodic publisher must not take the
/// writer down because the lake was briefly unreachable.
async fn publish_and_gc(
    handle: &IndexerHandle,
    lake_cfg: &LakeConfig,
    keep: u32,
    last_published_epoch: &Arc<Mutex<Option<u64>>>,
) {
    let indexer = handle.current();
    let last_epoch = *last_published_epoch
        .lock()
        .expect("last_published_epoch lock");
    let report = match publish_lake(&indexer, lake_cfg, last_epoch).await {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(
                target: "escurel",
                error = %e,
                "periodic publish: publish_lake failed"
            );
            return;
        }
    };
    if report.skipped {
        tracing::debug!(target: "escurel", "periodic publish: clean, nothing to publish");
        return;
    }
    *last_published_epoch
        .lock()
        .expect("last_published_epoch lock") = Some(report.epoch);
    tracing::info!(
        target: "escurel",
        snapshot_id = report.snapshot_id,
        epoch = report.epoch,
        pages = report.pages,
        blocks = report.blocks,
        "periodic publish: published a new snapshot"
    );
    match gc_lake_snapshots(&indexer, lake_cfg, keep).await {
        Ok(pruned) => {
            if pruned > 0 {
                tracing::info!(target: "escurel", pruned, "periodic publish: pruned old snapshots");
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "escurel",
                error = %e,
                "periodic publish: gc_lake_snapshots failed (publish itself still committed)"
            );
        }
    }
}

/// Handle to a spawned [`PublishTask`]. Signals shutdown and awaits the
/// loop task — same shape as
/// [`crate::snapshot_refresh::RefreshHandle`].
pub struct PublishHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl PublishHandle {
    /// Signal the loop to stop and await it.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}
