//! A periodic "still working" line for the slow steps of a boot.
//!
//! A writer that adopts a lake over a slow filesystem (Google Drive: ~1.6 s per file operation) can be
//! silent for a quarter of an hour, and a silent process is indistinguishable from a hung one to whoever
//! is watching the rollout. [`with_progress`] wraps one such step and logs, every [`PROGRESS_EVERY`], which
//! phase is running and for how long. It states facts it has (the phase, the elapsed time), never a
//! guess at a percentage.

use std::future::Future;
use std::time::Duration;

/// How often a slow boot step reports that it is still running.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(30);

/// Run `fut`, calling `on_tick(elapsed)` every `every` until it completes. The ticks run on their own
/// task, so they keep coming while `fut` blocks a worker thread on a synchronous call (a DuckDB query
/// over a slow filesystem does exactly that) as long as the runtime has another worker.
pub async fn with_progress_every<F, T>(
    every: Duration,
    on_tick: impl Fn(Duration) + Send + 'static,
    fut: F,
) -> T
where
    F: Future<Output = T>,
{
    let started = tokio::time::Instant::now();
    let ticker = tokio::spawn(async move {
        let mut tick = tokio::time::interval_at(started + every, every);
        loop {
            tick.tick().await;
            on_tick(started.elapsed());
        }
    });
    // Aborted on the way out, however `fut` ends; a dropped (cancelled) call stops the ticker too.
    let _stop = AbortOnDrop(ticker);
    fut.await
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// [`with_progress_every`] at [`PROGRESS_EVERY`], logging one `info` line per tick under the
/// `escurel_boot` target: the phase and the seconds it has been running.
pub async fn with_progress<F, T>(phase: &'static str, fut: F) -> T
where
    F: Future<Output = T>,
{
    with_progress_every(
        PROGRESS_EVERY,
        move |elapsed| {
            tracing::info!(
                target: "escurel_boot",
                phase,
                elapsed_s = elapsed.as_secs(),
                "boot: {phase} is still running"
            );
        },
        fut,
    )
    .await
}

/// Open the tenant's single-file index with the boot step reported. On a fresh data directory
/// (a new pod, a wiped volume, `ESCUREL_REBUILD_INDEX_ON_BOOT=always`) `open` scans every lane page for
/// legacy kinds and rebuilds the whole index from the lane store, which over Drive is the longest silent
/// phase of a boot (19-21 minutes on heron-escurel, rehearsal #982). `every` and `on_tick` are the
/// [`with_progress_every`] arguments, so a test can use a short interval.
pub async fn open_index_with_progress(
    store: &escurel_index::snapshot::SingleFileStore,
    every: Duration,
    on_tick: impl Fn(Duration) + Send + 'static,
) -> Result<escurel_index::snapshot::OpenedIndex, escurel_index::snapshot::SnapshotError> {
    with_progress_every(
        every,
        on_tick,
        escurel_index::snapshot::IndexStore::open(store),
    )
    .await
}

/// [`open_index_with_progress`] at [`PROGRESS_EVERY`], logging one `info` line per tick under the
/// `escurel_boot` target.
pub async fn open_index(
    store: &escurel_index::snapshot::SingleFileStore,
) -> Result<escurel_index::snapshot::OpenedIndex, escurel_index::snapshot::SnapshotError> {
    let phase = "open the index (a fresh data directory scans and rebuilds it from the lanes)";
    open_index_with_progress(store, PROGRESS_EVERY, move |elapsed| {
        tracing::info!(
            target: "escurel_boot",
            phase,
            elapsed_s = elapsed.as_secs(),
            "boot: {phase} is still running"
        );
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ticks_arrive_while_the_step_is_running_and_stop_when_it_ends() {
        let ticks = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&ticks);
        let out = with_progress_every(
            Duration::from_millis(20),
            move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
            async {
                tokio::time::sleep(Duration::from_millis(210)).await;
                7
            },
        )
        .await;
        assert_eq!(out, 7, "the step's own result comes through");
        let during = ticks.load(Ordering::SeqCst);
        assert!(
            (5..=11).contains(&during),
            "about one tick per interval while the step runs, got {during}"
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            during,
            "no tick after the step has finished"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ticks_keep_coming_while_the_step_blocks_a_worker_thread() {
        // The shape of a DuckDB call over a slow filesystem: synchronous, holding the thread.
        let ticks = Arc::new(AtomicU64::new(0));
        let seen = Arc::clone(&ticks);
        with_progress_every(
            Duration::from_millis(20),
            move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
            async {
                std::thread::sleep(Duration::from_millis(220));
            },
        )
        .await;
        assert!(
            ticks.load(Ordering::SeqCst) >= 4,
            "the ticker is not starved by a blocked worker, got {}",
            ticks.load(Ordering::SeqCst)
        );
    }
}
