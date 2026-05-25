//! M4.1 integration tests for `escurel-crdt`.
//!
//! These tests exercise the real Loro 1.x engine against a real
//! DuckDB file laid out by `escurel-index`'s `Migrator`. There are
//! no mocks at the LiveDoc / backend boundary — that is the
//! boundary these tests exist to cover (see `CLAUDE.md` principle
//! 2).
//!
//! The DuckDB connection is shared between the migrator and the
//! `DuckdbCrdtBackend` via `Arc<Mutex<Connection>>` because a
//! second `Connection::open` on the same file can return a stale
//! snapshot (see
//! `docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md`).

use std::sync::Arc;

use anyhow::Result;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend, LiveDoc, Op};
use escurel_index::schema::Migrator;
use loro::{ExportMode, LoroDoc};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Fixture: a fresh DuckDB file under a TempDir with the v1 schema
/// applied. Returns the `Arc<Mutex<Connection>>` for sharing with
/// `DuckdbCrdtBackend` and assertion helpers.
fn fresh_db() -> Result<(TempDir, Arc<Mutex<Connection>>)> {
    let dir = TempDir::new()?;
    let path = dir.path().join("tenant.duckdb");
    let conn = Connection::open(&path)?;
    Migrator::up(&conn)?;
    Ok((dir, Arc::new(Mutex::new(conn))))
}

/// A test-only "client" simulating one editor: holds its own
/// `LoroDoc`, applies local mutations, and exports incremental
/// updates that the actor's doc can import without losing context.
///
/// Using a persistent client (rather than scratch docs per op)
/// matches what a real WS client would do: every op anchors to
/// previous ops the actor has already seen, because all ops
/// originate from the same peer's oplog.
struct Client {
    doc: LoroDoc,
    /// Frontier vector after the last exported op. Used as the
    /// `from` argument so each export is incremental.
    vv: loro::VersionVector,
}

impl Client {
    fn new() -> Self {
        let doc = LoroDoc::new();
        let vv = doc.oplog_vv();
        Self { doc, vv }
    }

    /// Apply a local text insert and return the incremental update
    /// blob (a Loro op) covering only this change.
    fn insert(&mut self, pos: usize, text: &str) -> Op {
        self.doc.get_text("body").insert(pos, text).unwrap();
        self.doc.commit();
        let update = self.doc.export(ExportMode::updates(&self.vv)).unwrap();
        self.vv = self.doc.oplog_vv();
        Op::from(update)
    }

    fn body_len(&self) -> usize {
        self.doc.get_text("body").len_unicode()
    }
}

#[tokio::test]
async fn create_livedoc_and_apply_text_op_round_trips() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn));
    let doc = LiveDoc::open(backend.clone(), "page-a").await?;

    let mut client = Client::new();
    let op = client.insert(0, "hello");
    let v = doc.apply_op(op).await?;

    assert_eq!(v.as_str(), "v1");
    assert_eq!(doc.current_content().await, "hello");
    Ok(())
}

#[tokio::test]
async fn snapshot_then_close_then_reopen_replays_content() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn));

    let doc = LiveDoc::open(backend.clone(), "page-b").await?;
    let mut client = Client::new();
    let op1 = client.insert(0, "alpha ");
    doc.apply_op(op1).await?;

    let op2 = client.insert(client.body_len(), "beta");
    let v_after = doc.apply_op(op2).await?;
    assert_eq!(v_after.as_str(), "v2");

    let final_v = doc.close(true).await?;
    assert_eq!(final_v.as_str(), "v2");

    let reopened = LiveDoc::open(backend.clone(), "page-b").await?;
    assert_eq!(reopened.current_content().await, "alpha beta");
    Ok(())
}

#[tokio::test]
async fn multiple_ops_persisted_in_crdt_ops_table_in_order() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));
    let doc = LiveDoc::open(backend.clone(), "page-c").await?;

    let mut client = Client::new();
    for chunk in ["one ", "two ", "three"] {
        let pos = client.body_len();
        let op = client.insert(pos, chunk);
        doc.apply_op(op).await?;
    }
    let _ = doc.close(false).await?;

    // Assert the crdt_ops table has 3 rows for page-c, in HLC order.
    let guard = conn.lock().await;
    let mut stmt = guard.prepare("SELECT hlc FROM crdt_ops WHERE page_id = ? ORDER BY hlc ASC")?;
    let rows: Vec<i64> = stmt
        .query_map(["page-c"], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 3);
    assert!(
        rows.windows(2).all(|w| w[0] < w[1]),
        "hlc must be monotonic: {rows:?}"
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_apply_ops_serialise_through_actor() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));
    let doc = Arc::new(LiveDoc::open(backend.clone(), "page-d").await?);

    // Spawn N concurrent tasks each applying one insert from an
    // independent client. Independent clients model the
    // multi-editor case: their ops merge as CRDTs without any
    // ordering between them. The actor must serialise the writes
    // through the single LoroDoc; the final state contains every
    // chunk.
    let n = 8;
    let mut handles = Vec::new();
    for i in 0..n {
        let doc = doc.clone();
        handles.push(tokio::spawn(async move {
            let mut client = Client::new();
            let chunk = format!("[{i}]");
            let op = client.insert(0, &chunk);
            doc.apply_op(op).await
        }));
    }
    for h in handles {
        h.await??;
    }

    // All N ops must be in the op log.
    let guard = conn.lock().await;
    let count: i64 = guard.query_row(
        "SELECT count(*) FROM crdt_ops WHERE page_id = ?",
        ["page-d"],
        |row| row.get(0),
    )?;
    assert_eq!(count, n as i64);
    drop(guard);

    // Final content contains every chunk (CRDT merge).
    let content = doc.current_content().await;
    for i in 0..n {
        assert!(
            content.contains(&format!("[{i}]")),
            "missing chunk {i} in {content:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn reopening_with_only_snapshot_returns_correct_state() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));

    let doc = LiveDoc::open(backend.clone(), "page-e").await?;
    let mut client = Client::new();
    let op = client.insert(0, "snapshot-only");
    doc.apply_op(op).await?;
    let _ = doc.close(true).await?;

    // Simulate compaction: delete the op rows for page-e; only the
    // snapshot row remains.
    {
        let guard = conn.lock().await;
        guard.execute("DELETE FROM crdt_ops WHERE page_id = ?", ["page-e"])?;
    }

    let reopened = LiveDoc::open(backend.clone(), "page-e").await?;
    assert_eq!(reopened.current_content().await, "snapshot-only");
    Ok(())
}

#[tokio::test]
async fn reopening_with_snapshot_plus_ops_replays_to_correct_state() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));

    // Phase 1: write + snapshot "hello".
    let doc = LiveDoc::open(backend.clone(), "page-f").await?;
    let mut client = Client::new();
    let op1 = client.insert(0, "hello");
    doc.apply_op(op1).await?;
    let _ = doc.close(true).await?;

    // Phase 2: reopen, apply incremental op, close WITHOUT snapshot.
    // A new client simulates a different editor session.
    let doc = LiveDoc::open(backend.clone(), "page-f").await?;
    let mut client = Client::new();
    // Sync the new client to the doc's existing state so its
    // "world" anchor is consistent with the actor's.
    let snap = {
        let guard = conn.lock().await;
        let bytes: Vec<u8> = guard.query_row(
            "SELECT snapshot_bytes FROM crdt_snapshots WHERE page_id = ? ORDER BY snapshot_hlc DESC LIMIT 1",
            ["page-f"],
            |row| row.get(0),
        )?;
        bytes
    };
    client.doc.import(&snap).unwrap();
    client.vv = client.doc.oplog_vv();
    let op2 = client.insert(client.body_len(), ", world");
    doc.apply_op(op2).await?;
    let _ = doc.close(false).await?;

    // Phase 3: reopen and assert combined snapshot+ops state.
    let reopened = LiveDoc::open(backend.clone(), "page-f").await?;
    assert_eq!(reopened.current_content().await, "hello, world");
    Ok(())
}

#[tokio::test]
async fn empty_page_open_returns_empty_doc() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn));
    let doc = LiveDoc::open(backend.clone(), "never-written").await?;
    assert_eq!(doc.current_content().await, "");
    Ok(())
}

/// Codex P2 (PR M4.5b): reopening an already-snapshotted page and
/// closing immediately with commit=true used to dup-PK on
/// `crdt_snapshots(page_id, snapshot_hlc)`. The fix: skip the
/// snapshot when no ops were applied during the session.
#[tokio::test]
async fn close_commit_after_reopen_without_ops_does_not_dup_snapshot() -> Result<()> {
    let (_dir, conn) = fresh_db()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&conn)));

    // Phase 1: write one op, close with commit=true → snapshot at hlc=1.
    let mut client = Client::new();
    let doc = LiveDoc::open(backend.clone(), "page-rg").await?;
    let op1 = client.insert(0, "x");
    doc.apply_op(op1).await?;
    let _ = doc.close(true).await?;

    // Phase 2: reopen and immediately close with commit=true — no
    // ops applied this session. Must succeed without trying to
    // insert a second snapshot at the same hlc.
    let doc2 = LiveDoc::open(backend.clone(), "page-rg").await?;
    let _ = doc2.close(true).await?;

    // Confirm exactly one snapshot row remains.
    let n: i64 = conn.lock().await.query_row(
        "SELECT count(*) FROM crdt_snapshots WHERE page_id = ?",
        ["page-rg"],
        |row| row.get(0),
    )?;
    assert_eq!(n, 1, "snapshot must not be duplicated on no-op close");
    Ok(())
}
