//! In-memory session registry shared by every live-CRDT transport
//! on the gateway (HTTP MCP, gRPC bidi, WebSocket).
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
//! The close path removes the entry, then `Arc::try_unwrap`s the
//! sole remaining `Arc<LiveDoc>` to satisfy `LiveDoc::close`'s
//! `self` parameter. If a transport leaked another `Arc` clone
//! (e.g. by parking it on a background task), close falls back to
//! a `RuntimeBusy` error rather than blocking on the missing
//! drop.

use std::sync::Arc;

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
    /// (WS / gRPC bidi, M4.3+) can authorise ops without hitting
    /// the indexer. The HTTP MCP dispatcher doesn't read it
    /// directly — the unit test in this module exercises the
    /// round-trip — but the release build does not run unit
    /// tests, so dead-code allow keeps clippy quiet without
    /// dropping the field that the upcoming transports require.
    #[allow(dead_code)] // M4.3 WS / gRPC bidi consumer.
    page_id: String,
    doc: Arc<LiveDoc>,
    // Held for the lifetime of the session; dropped when the
    // entry is removed from the registry.
    _guard: Option<SessionGuard>,
}

/// Per-process session registry.
#[derive(Default)]
pub struct SessionManager {
    entries: DashMap<String, Entry>,
    /// Reverse lookup `page_id → session_id` so [`Self::open`] can
    /// enforce one session per page (the spec's "one LiveDoc actor
    /// per page" rule). Kept in sync with `entries` under each
    /// mutating call.
    pages: DashMap<String, String>,
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

    /// `close` couldn't reclaim sole ownership of the `LiveDoc`
    /// — another transport leaked an `Arc<LiveDoc>` clone past
    /// the registry's removal. The actor stays alive (the clone
    /// can still call `apply_op`); the session id is gone from
    /// the registry, so future `apply` / `close` calls 404.
    #[error("livedoc handle still referenced elsewhere; cannot close")]
    StillReferenced,

    /// Errors bubbled up from [`LiveDoc`] (Loro / DuckDB).
    #[error("livedoc error: {0}")]
    LiveDoc(#[from] escurel_crdt::Error),
}

impl SessionManager {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        let reservation = self.pages.entry(page_id.to_owned());
        if let dashmap::mapref::entry::Entry::Occupied(_) = reservation {
            return Err(SessionError::AlreadyOpen(page_id.to_owned()));
        }
        let session_id = format!("sess_{}", Ulid::new());
        reservation.or_insert_with(|| session_id.clone());

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
        // `LiveDoc::close` consumes `self`, so we need sole
        // ownership of the Arc. Under normal use (HTTP MCP only)
        // the entry holds the only strong count.
        let doc = Arc::try_unwrap(entry.doc).map_err(|_| SessionError::StillReferenced)?;
        let v = doc.close(commit).await?;
        // `entry._guard` already dropped when we destructured the
        // entry above; the semaphore slot is free.
        Ok(v)
    }

    /// Look up the `page_id` an open session is attached to. Used
    /// by the live transports (WS, gRPC bidi) to authorise ops
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
    /// the live transports (gRPC bidi attach in M4.3, WS attach +
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
}
