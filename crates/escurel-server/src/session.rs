//! In-memory session registry shared by every live-CRDT transport
//! on the gateway (HTTP MCP, WebSocket).
//!
//! Each entry maps a `sess_<ulid>` id to a [`LiveDoc`] actor plus
//! the [`SessionGuard`] returned by
//! [`QuotaManager::try_acquire_session`]. Dropping the guard
//! releases the per-tenant `concurrent_sessions` semaphore slot, so
//! callers must keep it inside the registry for the lifetime of
//! the editing session and release it on `close_session`.
//!
//! The registry is process-local (no Redis, no Vault), matching
//! the M4 single-replica deployment: live editing on a page is
//! serialised through one box.
//!
//! ## Concurrency model
//!
//! The `LiveDoc` itself is an actor: every call goes through an
//! mpsc channel, so multiple concurrent callers can already share
//! one `LiveDoc` instance via `&LiveDoc`. We wrap the LiveDoc in
//! an [`Arc`] so the apply path can obtain a cheap clone of the
//! handle and release the DashMap shard lock before awaiting the
//! actor — that way concurrent applies on different sessions
//! don't contend on the registry, only on their own actor.
//!
//! The close path removes the entry, then closes through the
//! `Arc<LiveDoc>` directly — `LiveDoc::close` takes `&self` and
//! terminates the actor via its `Command::Close`, so no sole-
//! ownership reclaim is needed and an outstanding `Arc` clone (e.g.
//! an in-flight `apply`) can no longer wedge close.

use std::sync::Arc;

use std::time::{Duration, Instant};

use dashmap::DashMap;
use escurel_crdt::{CrdtBackend, LiveDoc, Op, Version};
use escurel_quota::SessionGuard;
use thiserror::Error;
use ulid::Ulid;

/// One live editing session: the `LiveDoc` actor + the quota
/// guard.
struct Entry {
    /// The page this session edits. Surfaced via
    /// [`SessionManager::page_id_of`] so the live transports
    /// (WS attach, M4.3+) can authorise ops without hitting
    /// the indexer. The HTTP MCP dispatcher doesn't read it
    /// directly — the unit test in this module exercises the
    /// round-trip — but the release build does not run unit
    /// tests, so dead-code allow keeps clippy quiet without
    /// dropping the field that the upcoming transports require.
    #[allow(dead_code)] // M4.3 WS attach consumer.
    page_id: String,
    doc: Arc<LiveDoc>,
    // Held for the lifetime of the session; dropped when the
    // entry is removed from the registry.
    _guard: Option<SessionGuard>,
    /// Last time this session saw activity (open or apply). Drives
    /// idle eviction so a client that drops its transport without a
    /// clean `close` (e.g. a crash) can't lock the page forever.
    /// `Mutex` for interior mutability behind DashMap's shared refs.
    last_activity: std::sync::Mutex<Instant>,
}

impl Entry {
    fn touch(&self) {
        if let Ok(mut t) = self.last_activity.lock() {
            *t = Instant::now();
        }
    }

    fn idle_for(&self, now: Instant) -> Duration {
        self.last_activity
            .lock()
            .map(|t| now.saturating_duration_since(*t))
            .unwrap_or_default()
    }
}

/// Default idle TTL after which an inactive session may be evicted
/// to recover a page locked by a crashed client. Generous: a live
/// editor reattaching after a transient transport drop is well
/// within this window.
pub const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

/// Per-process session registry.
pub struct SessionManager {
    entries: DashMap<String, Entry>,
    /// Reverse lookup `page_id → session_id` so [`Self::open`] can
    /// enforce one session per page (the spec's "one LiveDoc actor
    /// per page" rule). Kept in sync with `entries` under each
    /// mutating call.
    pages: DashMap<String, String>,
    /// How long a session may sit idle before [`Self::open`] (or
    /// [`Self::evict_idle`]) is allowed to reclaim its page.
    idle_ttl: Duration,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self {
            entries: DashMap::new(),
            pages: DashMap::new(),
            idle_ttl: DEFAULT_IDLE_TTL,
        }
    }
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("open_sessions", &self.entries.len())
            .finish()
    }
}

/// Errors returned by [`SessionManager`].
#[derive(Debug, Error)]
pub enum SessionError {
    /// `apply` / `close` was called with a session id the registry
    /// doesn't know — either it was never opened, or it has
    /// already been closed (and the slot released).
    #[error("unknown session: {0}")]
    UnknownSession(String),

    /// `open` was called on a `page_id` that already has a live
    /// session. The spec's `LiveDoc` actor is one-per-page (see
    /// `docs/spec/storage.md` §The Loro engine); a second
    /// independent actor for the same page would write ops with
    /// HLCs that collide on `(page_id, op_id)` and would not
    /// converge (Loro ops are peer-anchored). M4.4 will add WS
    /// "attach to open session by id" so multiple clients can
    /// share one actor; HTTP MCP requires exclusive ownership
    /// today (codex review on PR M4.5b).
    #[error("page `{0}` already has an open session")]
    AlreadyOpen(String),

    /// Errors bubbled up from [`LiveDoc`] (Loro / DuckDB).
    #[error("livedoc error: {0}")]
    LiveDoc(#[from] escurel_crdt::Error),
}

impl SessionManager {
    /// Build an empty registry with the default idle TTL.
    /// Equivalent to [`Default::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry with a custom idle-eviction TTL. Only tests
    /// need a non-default window (a tiny/zero TTL to drive eviction
    /// deterministically); production always uses [`DEFAULT_IDLE_TTL`]
    /// via [`Self::new`].
    #[cfg(test)]
    #[must_use]
    pub fn with_idle_ttl(idle_ttl: Duration) -> Self {
        Self {
            idle_ttl,
            ..Self::default()
        }
    }

    /// Open a new session on `page_id`, returning the freshly
    /// minted session id and the current head version. Callers
    /// must have already acquired a [`SessionGuard`] from the
    /// per-tenant quota manager (or `None` when no quota is
    /// configured); the registry takes ownership and drops it on
    /// [`close`](Self::close).
    pub async fn open(
        &self,
        backend: Arc<dyn CrdtBackend>,
        page_id: &str,
        guard: Option<SessionGuard>,
    ) -> Result<(String, Version), SessionError> {
        // One session per page (see [`SessionError::AlreadyOpen`]).
        // The DashMap `entry` API holds a shard write lock for the
        // duration of the check + reservation, so two concurrent
        // open() calls on the same page can't both win the race.
        // We reserve with a placeholder session_id and overwrite
        // once the LiveDoc opens; on LiveDoc::open failure we drop
        // the reservation. Note: dropping `guard` here releases the
        // quota slot the caller acquired — correct, because the
        // request did not actually open a session.
        // If a session already holds this page, opportunistically
        // evict it when it has gone idle past the TTL before
        // rejecting. That's the backstop for a client that dropped its
        // transport without a clean close (a crash) — otherwise the
        // page-reservation would lock it forever. We read+drop the
        // reverse-lookup ref (no await held) then evict outside any
        // shard lock.
        let stale_sid = self.pages.get(page_id).map(|r| r.value().clone());
        if let Some(stale_sid) = stale_sid {
            let evicted = self.evict_if_idle(&stale_sid, page_id, self.idle_ttl).await;
            if !evicted {
                return Err(SessionError::AlreadyOpen(page_id.to_owned()));
            }
        }

        let session_id = format!("sess_{}", Ulid::new());
        // Atomically reserve the page; if another open() won the race
        // between the idle check above and here, reject.
        match self.pages.entry(page_id.to_owned()) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(SessionError::AlreadyOpen(page_id.to_owned()));
            }
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(session_id.clone());
            }
        }

        // Open outside the shard lock — LiveDoc::open touches DuckDB
        // and could otherwise hold the lock across IO.
        let doc = match LiveDoc::open(backend, page_id).await {
            Ok(d) => d,
            Err(e) => {
                // Roll the reservation back; otherwise the page
                // would be wedged.
                self.pages.remove(page_id);
                return Err(SessionError::from(e));
            }
        };
        // `current_content` forces the actor to drain its replay
        // buffer; we ignore the value but the call guarantees the
        // doc is ready to accept ops before we return.
        let _ = doc.current_content().await;

        self.entries.insert(
            session_id.clone(),
            Entry {
                page_id: page_id.to_owned(),
                doc: Arc::new(doc),
                _guard: guard,
                last_activity: std::sync::Mutex::new(Instant::now()),
            },
        );
        // v1 always reports `v0` as the head at open time. The
        // doc's real version surfaces on the first `apply_op`
        // reply. M4.6 will switch to real HLC strings sourced
        // from `backend.max_hlc(page_id)`.
        Ok((session_id, Version::from_op_count(0)))
    }

    /// Apply a Loro op blob to an open session. Returns the
    /// post-merge version.
    pub async fn apply(&self, session_id: &str, op: Op) -> Result<Version, SessionError> {
        // Take a cheap clone of the Arc so we drop the DashMap
        // shard lock before awaiting the actor reply.
        let doc = {
            let entry = self
                .entries
                .get(session_id)
                .ok_or_else(|| SessionError::UnknownSession(session_id.to_owned()))?;
            entry.touch();
            Arc::clone(&entry.doc)
        };
        Ok(doc.apply_op(op).await?)
    }

    /// Close a session, optionally snapshotting the doc. Drops
    /// the per-tenant quota guard as the entry is removed.
    pub async fn close(&self, session_id: &str, commit: bool) -> Result<Version, SessionError> {
        // Remove first so the registry no longer hands out Arc
        // clones to other callers.
        let entry = self
            .entries
            .remove(session_id)
            .map(|(_, e)| e)
            .ok_or_else(|| SessionError::UnknownSession(session_id.to_owned()))?;
        // Release the page reservation so a subsequent open() on
        // the same page can succeed.
        self.pages.remove(&entry.page_id);
        // `LiveDoc::close` takes `&self` and terminates the actor via
        // its `Command::Close`, so we can close through the `Arc`
        // directly — no `Arc::try_unwrap`, which would wedge close with
        // `StillReferenced` if any in-flight `apply` still held a clone.
        let v = entry.doc.close(commit).await?;
        // `entry._guard` already dropped when we destructured the
        // entry above; the semaphore slot is free.
        Ok(v)
    }

    /// Evict the session `session_id` (attached to `page_id`) iff it
    /// has been idle for at least `ttl`. Returns `true` when it was
    /// evicted (or was already gone / not attached to this page —
    /// either way the page is free for the caller). An idle-evict is
    /// treated like an abandoned draft: the doc is discarded WITHOUT
    /// committing, mirroring `close(commit = false)`.
    async fn evict_if_idle(&self, session_id: &str, page_id: &str, ttl: Duration) -> bool {
        let now = Instant::now();
        // Inspect idle time without holding the ref across the await.
        let should_evict = match self.entries.get(session_id) {
            Some(entry) => entry.page_id == page_id && entry.idle_for(now) >= ttl,
            // No such session — the reverse-lookup is stale; the page
            // is effectively free.
            None => true,
        };
        if !should_evict {
            return false;
        }
        // Discard (commit = false). A removed/already-gone session is
        // fine — the page reservation is cleared regardless.
        let entry = self.entries.remove(session_id).map(|(_, e)| e);
        self.pages.remove(page_id);
        if let Some(entry) = entry {
            let _ = entry.doc.close(false).await;
        }
        true
    }

    /// Sweep all sessions idle for at least `ttl`, discarding each
    /// (no commit). Returns the number evicted. A periodic caller can
    /// run this as a background backstop; [`Self::open`] also evicts
    /// opportunistically on the contended page so a reconnecting
    /// editor is never locked out by a crashed predecessor.
    pub async fn evict_idle(&self, ttl: Duration) -> usize {
        let now = Instant::now();
        let stale: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|e| e.idle_for(now) >= ttl)
            .map(|e| (e.key().clone(), e.page_id.clone()))
            .collect();
        let mut evicted = 0;
        for (sid, page_id) in stale {
            if self.evict_if_idle(&sid, &page_id, ttl).await {
                evicted += 1;
            }
        }
        evicted
    }

    /// Look up the `page_id` an open session is attached to. Used
    /// by the live transports (WS attach) to authorise ops
    /// without round-tripping through the indexer. M4.4 wires
    /// the WS attach path through this accessor; the HTTP MCP
    /// dispatcher doesn't call it directly.
    #[must_use]
    pub fn page_id_of(&self, session_id: &str) -> Option<String> {
        self.entries.get(session_id).map(|e| e.page_id.clone())
    }

    /// Number of currently-open live sessions, for the
    /// `escurel_live_sessions_open` gauge (sampled at scrape time).
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.entries.len()
    }

    /// Read the current text content of an open session. Used by
    /// the live transports (WS attach +
    /// `op_ack` replies in M4.4) to populate the `content` field
    /// without forcing the caller to keep a parallel mirror of the
    /// doc.
    ///
    /// Returns `None` when the session id is unknown — the caller
    /// surfaces that as the spec's `unknown_session` issue. When
    /// the session is open but the underlying actor returned the
    /// empty string (e.g. just opened, never written), we still
    /// return `Some("")` to match the spec's `content: String`
    /// shape.
    pub async fn current_content(&self, session_id: &str) -> Option<String> {
        let doc = {
            let entry = self.entries.get(session_id)?;
            Arc::clone(&entry.doc)
        };
        Some(doc.current_content().await)
    }
}

#[cfg(test)]
mod tests {
    //! Inline unit tests that pin the registry's bookkeeping
    //! invariants. The full live-CRDT path is exercised by
    //! `tests/mcp_session_tools.rs`; this module covers the
    //! pure-Rust API surface only.

    use std::sync::Arc;

    use duckdb::Connection;
    use escurel_crdt::DuckdbCrdtBackend;
    use escurel_index::Migrator;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use super::*;

    fn backend() -> (TempDir, Arc<dyn CrdtBackend>) {
        let dir = TempDir::new().unwrap();
        let conn = Connection::open(dir.path().join("t.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();
        let shared = Arc::new(Mutex::new(conn));
        (dir, Arc::new(DuckdbCrdtBackend::new(shared)))
    }

    #[tokio::test]
    async fn page_id_of_round_trips_through_open() {
        let (_dir, b) = backend();
        let sm = SessionManager::new();
        let (sid, _v) = sm.open(b, "page-x", None).await.unwrap();
        assert_eq!(sm.page_id_of(&sid).as_deref(), Some("page-x"));
        // After close the lookup must return None (the slot is
        // gone and the quota guard, if any, is dropped).
        let _ = sm.close(&sid, false).await.unwrap();
        assert!(sm.page_id_of(&sid).is_none());
    }

    #[tokio::test]
    async fn close_unknown_session_is_unknown_error() {
        let sm = SessionManager::new();
        let err = sm.close("sess_does-not-exist", false).await.unwrap_err();
        assert!(matches!(err, SessionError::UnknownSession(_)));
    }

    #[tokio::test]
    async fn second_open_on_same_page_rejected() {
        let (_dir, b) = backend();
        let sm = SessionManager::new();
        let (sid_a, _) = sm.open(Arc::clone(&b), "page-x", None).await.unwrap();
        let err = sm.open(Arc::clone(&b), "page-x", None).await.unwrap_err();
        assert!(matches!(err, SessionError::AlreadyOpen(_)));
        // After close, a second open is allowed again.
        let _ = sm.close(&sid_a, false).await.unwrap();
        let _ = sm.open(b, "page-x", None).await.unwrap();
    }

    #[tokio::test]
    async fn close_succeeds_while_a_doc_arc_clone_is_held() {
        // Pins FIX 3: close must not depend on sole Arc ownership.
        // We clone the registry's `Arc<LiveDoc>` (simulating an
        // in-flight `apply` or a transport that parked a handle) and
        // hold it across `close`. The old `Arc::try_unwrap` path
        // would have failed here with `StillReferenced`; with
        // `LiveDoc::close(&self)` the close goes through cleanly.
        let (_dir, b) = backend();
        let sm = SessionManager::new();
        let (sid, _v) = sm.open(b, "page-held", None).await.unwrap();

        // Reach into the registry (same module) and clone the doc Arc.
        let held: Arc<escurel_crdt::LiveDoc> = {
            let entry = sm.entries.get(&sid).unwrap();
            Arc::clone(&entry.doc)
        };
        assert!(Arc::strong_count(&held) >= 2, "a second strong ref is held");

        // Close while the clone is still alive: must succeed.
        let v = sm.close(&sid, false).await.expect("close must not wedge");
        let _ = v; // a final Version is returned
        assert!(sm.page_id_of(&sid).is_none(), "entry removed");

        // The held clone's actor has terminated; a further close
        // through it is a benign Closed error, not a hang.
        let after = held.close(false).await;
        assert!(after.is_err(), "actor already terminated: {after:?}");
    }

    #[tokio::test]
    async fn current_content_round_trips() {
        let (_dir, b) = backend();
        let sm = SessionManager::new();
        let (sid, _) = sm.open(b, "page-content", None).await.unwrap();
        // Empty doc → empty content.
        assert_eq!(sm.current_content(&sid).await.as_deref(), Some(""));
        // Unknown id → None.
        assert!(sm.current_content("sess_nope").await.is_none());
        let _ = sm.close(&sid, false).await.unwrap();
    }

    #[tokio::test]
    async fn open_evicts_an_idle_session_instead_of_locking_the_page() {
        // FIX 2: a client that dropped its transport without close
        // (a crash) must not lock the page forever. With a zero TTL,
        // a fresh open() on the same page evicts the stale session
        // rather than returning AlreadyOpen.
        let (_dir, b) = backend();
        let sm = SessionManager::with_idle_ttl(Duration::ZERO);

        let (sid_a, _) = sm.open(Arc::clone(&b), "page-lock", None).await.unwrap();
        // Simulate a silent transport drop: no close() is called.

        // A second open on the same page succeeds by evicting the idle
        // predecessor (TTL is zero, so it's immediately idle).
        let (sid_b, _) = sm
            .open(Arc::clone(&b), "page-lock", None)
            .await
            .expect("idle session must be evicted, not locked out");
        assert_ne!(sid_a, sid_b, "a new session was minted");

        // The stale session id is gone from the registry.
        assert!(sm.page_id_of(&sid_a).is_none());
        // The new one is live and attached to the page.
        assert_eq!(sm.page_id_of(&sid_b).as_deref(), Some("page-lock"));
        assert_eq!(sm.open_count(), 1, "exactly one live session");

        let _ = sm.close(&sid_b, false).await.unwrap();
    }

    #[tokio::test]
    async fn active_session_is_not_evicted_on_contended_open() {
        // The eviction backstop must not steal a page from a genuinely
        // active editor: with a generous TTL, a contended open still
        // gets AlreadyOpen.
        let (_dir, b) = backend();
        let sm = SessionManager::with_idle_ttl(Duration::from_secs(3600));
        let (_sid, _) = sm.open(Arc::clone(&b), "page-busy", None).await.unwrap();
        let err = sm.open(b, "page-busy", None).await.unwrap_err();
        assert!(matches!(err, SessionError::AlreadyOpen(_)), "{err}");
    }

    #[tokio::test]
    async fn evict_idle_sweeps_stale_sessions() {
        let (_dir, b) = backend();
        let sm = SessionManager::with_idle_ttl(Duration::from_secs(3600));
        let (_a, _) = sm.open(Arc::clone(&b), "p1", None).await.unwrap();
        let (_b, _) = sm.open(Arc::clone(&b), "p2", None).await.unwrap();
        assert_eq!(sm.open_count(), 2);
        // A zero-TTL sweep evicts everything idle (all of it).
        let n = sm.evict_idle(Duration::ZERO).await;
        assert_eq!(n, 2);
        assert_eq!(sm.open_count(), 0);
        // Pages are freed → reopen succeeds.
        let _ = sm.open(b, "p1", None).await.unwrap();
    }
}
