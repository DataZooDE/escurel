//! `LiveDoc`: the per-page Tokio actor that owns a `LoroDoc`.
//!
//! Every `apply_op` / `current_content` / `close` call goes
//! through an mpsc channel, so the LoroDoc is touched from exactly
//! one task. That's the only safe pattern for a non-`Sync`
//! engine (Loro's `LoroDoc` is `Send` but not `Sync`) under
//! concurrent async writers.
//!
//! On `open`: load the latest snapshot (if any) + replayed ops
//! from the backend, build a fresh `LoroDoc`, import them in
//! order, then spawn the actor task.
//!
//! On `apply_op`: actor imports the bytes into the engine, derives
//! a monotonic HLC (op-count for v1), writes the op row through
//! the backend, and returns the new `Version`.
//!
//! On `close(commit=true)`: actor exports a snapshot via
//! `ExportMode::Snapshot`, writes it through the backend, then
//! exits.

use std::sync::Arc;

use loro::{ExportMode, LoroDoc};
use tokio::sync::{mpsc, oneshot};

use crate::{CrdtBackend, Error, Op, Snapshot, Version};

/// One-shot read of a page's current CRDT content (#246): hydrate a `LoroDoc`
/// from the backend's latest snapshot + post-snapshot ops and return its `body`
/// text (the whole-page markdown). `None` when the page has no CRDT state yet.
/// Lighter than [`LiveDoc::open`] — no actor task; use it for a read-only peek
/// (e.g. `update_page`'s conflict `head_content`).
pub async fn hydrate_content(
    backend: &Arc<dyn CrdtBackend>,
    page_id: &str,
) -> Result<Option<String>, Error> {
    let Some((snap, ops)) = backend.load(page_id).await? else {
        return Ok(None);
    };
    let doc = LoroDoc::new();
    if !snap.as_bytes().is_empty() {
        doc.import(snap.as_bytes())?;
    }
    for op in ops {
        doc.import(op.as_bytes())?;
    }
    Ok(Some(doc.get_text("body").to_string()))
}

/// A single live CRDT session attached to one page.
///
/// Cheap to clone via the underlying mpsc sender; clone if you
/// need to drive the actor from multiple call sites. Closing one
/// clone (via [`LiveDoc::close`]) closes the actor for all.
pub struct LiveDoc {
    tx: mpsc::Sender<Command>,
}

/// Internal actor command set. Each variant carries a `oneshot`
/// reply slot so the caller awaits the actor's result.
enum Command {
    ApplyOp(Op, oneshot::Sender<Result<Version, Error>>),
    ReadContent(oneshot::Sender<String>),
    Close(bool, oneshot::Sender<Result<Version, Error>>),
}

impl LiveDoc {
    /// Open a `LiveDoc` for `page_id`.
    ///
    /// Loads the latest snapshot + post-snapshot ops from
    /// `backend`, hydrates a fresh `LoroDoc`, and spawns the
    /// actor task.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Loro`] if any persisted op or snapshot
    /// fails to import (corrupted blob, version mismatch with the
    /// engine), and [`Error::Duckdb`] if the backend's `load`
    /// fails.
    pub async fn open(
        backend: Arc<dyn CrdtBackend>,
        page_id: &str,
        seed: Option<&str>,
    ) -> Result<Self, Error> {
        let doc = LoroDoc::new();

        let loaded = backend.load(page_id).await?;
        let had_state = loaded
            .as_ref()
            .is_some_and(|(snap, ops)| !snap.as_bytes().is_empty() || !ops.is_empty());
        if let Some((snap, ops)) = loaded {
            if !snap.as_bytes().is_empty() {
                doc.import(snap.as_bytes())?;
            }
            for op in ops {
                doc.import(op.as_bytes())?;
            }
        }

        // **Seed from the page when the CRDT knows nothing about it (#421).**
        //
        // A page written through `update_page` has no snapshot and no ops, so
        // without this the session starts EMPTY over a page full of content —
        // and `close_session {commit: true}` then writes the document's text
        // as the whole page. Every device joining sees a blank document, and
        // the first edit committed replaces what was there. No error is raised
        // anywhere: the write is well-formed, it is simply not the page.
        //
        // It is persisted rather than held in memory, because the next `open`
        // must find it: an in-memory-only seed hydrates correctly once, then
        // starts empty again on reopen as soon as the room has applied any op
        // of its own — the same bug, one session later, and harder to see.
        //
        // Persisted as a SNAPSHOT, not as an op, and the difference is not
        // bookkeeping. An op is somebody's edit; `list_op_authors` enumerates
        // them and reports each one's principal. The seed is the page's
        // existing content, whose author is whoever last wrote the page — so
        // recording it as an op adds an anonymous editor to the history
        // (`principal: null`) and tells a reader that someone unidentified
        // touched their page. It cost a test in `write_attribution` to notice.
        // A snapshot is what this actually is: the base state a session starts
        // from.
        if !had_state && let Some(text) = seed.filter(|t| !t.is_empty()) {
            doc.get_text("body").insert(0, text)?;
            let snap = crate::Snapshot::new(doc.export(ExportMode::Snapshot)?);
            backend.snapshot_next(page_id, &snap).await?;
        }

        // Seed op_count from the highest persisted hlc, not the
        // replay count: after a snapshot the replay loop sees zero
        // ops, but the next `apply_op` must still produce a fresh
        // `(page_id, op_id)` primary key. Using max_hlc preserves
        // that invariant.
        let op_count = u64::try_from(backend.max_hlc(page_id).await?).unwrap_or(0);

        // Bounded channel keeps backpressure honest: if the actor
        // can't keep up, callers wait. 64 is a per-page burst
        // window; tune later if profiling shows starvation.
        let (tx, rx) = mpsc::channel::<Command>(64);
        let page_id = page_id.to_owned();
        tokio::spawn(actor_loop(doc, backend, page_id, op_count, op_count, rx));

        Ok(Self { tx })
    }

    /// Apply a Loro op blob and return the new `Version`.
    ///
    /// Blocks until the actor has imported the op into the engine,
    /// persisted the op row, and replied with the new version.
    pub async fn apply_op(&self, op: Op) -> Result<Version, Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Command::ApplyOp(op, reply_tx))
            .await
            .map_err(|_| Error::Closed)?;
        reply_rx.await.map_err(|_| Error::Closed)?
    }

    /// Read the current text content of the `"body"` container.
    ///
    /// For M4.1 the engine exposes exactly one text container
    /// (`"body"`) — multi-container documents arrive when the
    /// markdown ↔ CRDT bridge lands.
    pub async fn current_content(&self) -> String {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(Command::ReadContent(reply_tx)).await.is_err() {
            return String::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Close the actor. If `commit == true`, the actor first
    /// exports a snapshot and persists it. Returns the final
    /// `Version` at which the doc was closed.
    ///
    /// Takes `&self` (not `self`): the actor loop terminates on the
    /// `Command::Close` it receives here (or when the last sender
    /// drops), so ownership of the handle isn't required. This frees
    /// callers from `Arc::try_unwrap` gymnastics when the doc is held
    /// behind a shared `Arc`.
    pub async fn close(&self, commit: bool) -> Result<Version, Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Command::Close(commit, reply_tx))
            .await
            .map_err(|_| Error::Closed)?;
        reply_rx.await.map_err(|_| Error::Closed)?
    }
}

/// Actor body: owns the `LoroDoc` and the backend handle for the
/// lifetime of one session. Exits when:
///   * a `Close` command is received, or
///   * the mpsc sender is dropped (no live handles remain).
async fn actor_loop(
    doc: LoroDoc,
    backend: Arc<dyn CrdtBackend>,
    page_id: String,
    initial_op_count: u64,
    mut op_count: u64,
    mut rx: mpsc::Receiver<Command>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::ApplyOp(op, reply) => {
                let result = handle_apply(&doc, &backend, &page_id, &mut op_count, op).await;
                let _ = reply.send(result);
            }
            Command::ReadContent(reply) => {
                let _ = reply.send(read_body(&doc));
            }
            Command::Close(commit, reply) => {
                let result =
                    handle_close(&doc, &backend, &page_id, initial_op_count, op_count, commit)
                        .await;
                let _ = reply.send(result);
                break;
            }
        }
    }
}

async fn handle_apply(
    doc: &LoroDoc,
    backend: &Arc<dyn CrdtBackend>,
    page_id: &str,
    op_count: &mut u64,
    op: Op,
) -> Result<Version, Error> {
    doc.import(op.as_bytes())?;
    // The backend allocates the hlc from the persisted maximum and inserts
    // under one lock. The actor no longer numbers its own ops: `op_count` was
    // seeded from `max_hlc` once at open, so a whole-page `update_page`
    // landing mid-session made it stale, and both writers then stamped
    // different content with the same version (F1.3). `op_count` survives
    // only as a "did this session change anything" marker for close.
    let hlc = backend.append_op_next(page_id, &op).await?;
    *op_count = u64::try_from(hlc).unwrap_or(*op_count + 1);
    Ok(Version::from_op_count(*op_count))
}

async fn handle_close(
    doc: &LoroDoc,
    backend: &Arc<dyn CrdtBackend>,
    page_id: &str,
    initial_op_count: u64,
    op_count: u64,
    commit: bool,
) -> Result<Version, Error> {
    // Skip the snapshot when no ops were applied this session: the
    // current in-memory state matches what we loaded from disk, so
    // a fresh snapshot at the same hlc would either be redundant
    // or collide on `(page_id, snapshot_hlc)` PK (codex review on
    // PR M4.5b).
    if commit && op_count > initial_op_count {
        let bytes = doc.export(ExportMode::Snapshot)?;
        // Snapshot AT the last op's hlc, not above it. The snapshot records
        // "the state as of op N" — it is not itself a new event, so
        // allocating a fresh hlc here would advance the head without any
        // content change and turn the version a client just received from
        // `apply_op` into a stale `base_version`.
        //
        // Safe against the `(page_id, snapshot_hlc)` primary key now that ops
        // allocate from the shared maximum: a snapshot already sitting at N
        // implies the maximum was at least N, so this session's op would have
        // been given N+1 rather than N.
        let hlc = i64::try_from(op_count).unwrap_or(i64::MAX);
        backend
            .snapshot(page_id, hlc, &Snapshot::new(bytes))
            .await?;
    }
    Ok(Version::from_op_count(op_count))
}

fn read_body(doc: &LoroDoc) -> String {
    doc.get_text("body").to_string()
}
