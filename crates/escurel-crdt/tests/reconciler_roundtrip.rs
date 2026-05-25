//! M4.6 integration tests for `escurel-crdt::reconciler`.
//!
//! These tests exercise the two-stage external-edit reconciler
//! against a real Loro engine, a real DuckDB file (via the
//! `escurel-index` migrator), and a real filesystem `LaneStore`.
//! There are no mocks at the LiveDoc / backend / storage boundary
//! — `CLAUDE.md` principle 2.
//!
//! The `CitationLookup` trait IS a boundary (citation queries
//! belong to `escurel-index`, which we cannot import from
//! `escurel-crdt` without crate-level coupling). Tests supply an
//! in-process `StubCitations` impl; the real `IndexerCitationLookup`
//! impl lands in the consuming crate (escurel-server / escurel-cli)
//! in a wiring PR.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_crdt::reconciler::{CitationLookup, Decision, ExternalEditReconciler};
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend, LiveDoc, Op};
use escurel_index::schema::Migrator;
use escurel_storage::{FsStore, Key, LaneStore};
use loro::{ExportMode, LoroDoc};
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Fixture tuple returned by [`fresh_env`]. Aliased to keep the
/// function signature under clippy's `type_complexity` lint without
/// burying meaning behind an opaque name.
type TestEnv = (TempDir, Arc<Mutex<Connection>>, Arc<dyn LaneStore>);

/// Fixture: a fresh DuckDB file + a fresh `FsStore` rooted at the
/// same TempDir. Both share the temp directory so a test only has to
/// hold one `_dir` guard.
fn fresh_env() -> Result<TestEnv> {
    let dir = TempDir::new()?;
    let db_path = dir.path().join("tenant.duckdb");
    let conn = Connection::open(&db_path)?;
    Migrator::up(&conn)?;
    let store_root = dir.path().join("store");
    std::fs::create_dir_all(&store_root)?;
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_root));
    Ok((dir, Arc::new(Mutex::new(conn)), store))
}

/// Helper: build the canonical markdown key the reconciler expects
/// for a given page id under tenant "t1".
fn page_key(page_id: &str) -> Key {
    Key::new("t1", format!("pages/{page_id}.md")).unwrap()
}

/// In-test `CitationLookup` that holds the set of cited page ids.
/// Real impl pulls from the `links` table via `escurel-index`.
#[derive(Default, Clone)]
struct StubCitations {
    cited: Arc<std::sync::Mutex<HashSet<String>>>,
}

impl StubCitations {
    fn empty() -> Self {
        Self::default()
    }

    fn with(pages: &[&str]) -> Self {
        let cited: HashSet<String> = pages.iter().map(|s| (*s).to_owned()).collect();
        Self {
            cited: Arc::new(std::sync::Mutex::new(cited)),
        }
    }
}

#[async_trait]
impl CitationLookup for StubCitations {
    async fn is_cited(&self, _tenant: &str, page_id: &str) -> Result<bool, anyhow::Error> {
        Ok(self.cited.lock().unwrap().contains(page_id))
    }
}

/// Helper: open a LiveDoc, apply one text insert from a fresh
/// persistent client, commit a snapshot.
async fn seed_snapshot(
    backend: Arc<dyn CrdtBackend>,
    page_id: &str,
    text: &str,
) -> Result<Vec<u8>> {
    let doc = LiveDoc::open(backend, page_id).await?;
    let client_doc = LoroDoc::new();
    let vv = client_doc.oplog_vv();
    client_doc.get_text("body").insert(0, text)?;
    client_doc.commit();
    let op_bytes = client_doc.export(ExportMode::updates(&vv))?;
    doc.apply_op(Op::from(op_bytes)).await?;
    let _ = doc.close(true).await?;
    Ok(text.as_bytes().to_owned())
}

/// Helper: read the most recent snapshot for `page_id`. Decodes the
/// raw Loro snapshot blob into a temporary `LoroDoc` so the test can
/// assert on the rendered body text.
async fn snapshot_body(conn: &Arc<Mutex<Connection>>, page_id: &str) -> Result<String> {
    let guard = conn.lock().await;
    let bytes: Vec<u8> = guard.query_row(
        "SELECT snapshot_bytes FROM crdt_snapshots WHERE page_id = ? \
         ORDER BY snapshot_hlc DESC LIMIT 1",
        [page_id],
        |row| row.get(0),
    )?;
    drop(guard);
    let doc = LoroDoc::new();
    doc.import(&bytes)?;
    Ok(doc.get_text("body").to_string())
}

/// Helper: count snapshot rows for a page.
async fn snapshot_count(conn: &Arc<Mutex<Connection>>, page_id: &str) -> Result<i64> {
    let guard = conn.lock().await;
    let n: i64 = guard.query_row(
        "SELECT count(*) FROM crdt_snapshots WHERE page_id = ?",
        [page_id],
        |row| row.get(0),
    )?;
    Ok(n)
}

#[tokio::test]
async fn uncited_page_external_edit_wins_and_replaces_snapshot() -> Result<()> {
    let (_dir, conn, store) = fresh_env()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));

    // Seed: a snapshot with "first version" body.
    seed_snapshot(backend.clone(), "page-uncited", "first version").await?;
    let key = page_key("page-uncited");
    store
        .write(&key, Bytes::from_static(b"first version"))
        .await?;

    // External edit: overwrite the on-disk markdown.
    store
        .write(&key, Bytes::from_static(b"externally edited body"))
        .await?;

    // No other page cites page-uncited.
    let citations: Arc<dyn CitationLookup> = Arc::new(StubCitations::empty());
    let reconciler = ExternalEditReconciler::new(backend.clone(), store.clone(), citations);

    let decision = reconciler.reconcile("t1", "page-uncited", &key).await?;
    assert!(matches!(decision, Decision::ExternalWins));

    // The new snapshot must reflect the externally edited body.
    let body = snapshot_body(&conn, "page-uncited").await?;
    assert_eq!(body, "externally edited body");
    // And a second snapshot row must exist (the replacement).
    assert_eq!(snapshot_count(&conn, "page-uncited").await?, 2);
    Ok(())
}

#[tokio::test]
async fn cited_page_external_edit_loses_and_snapshot_stays() -> Result<()> {
    let (_dir, conn, store) = fresh_env()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));

    seed_snapshot(backend.clone(), "page-cited", "canonical").await?;
    let key = page_key("page-cited");
    store.write(&key, Bytes::from_static(b"canonical")).await?;

    // External edit on disk that we will NOT honour.
    store
        .write(&key, Bytes::from_static(b"sneaky on-disk edit"))
        .await?;

    // Another page links INTO page-cited; the snapshot is canonical.
    let citations: Arc<dyn CitationLookup> = Arc::new(StubCitations::with(&["page-cited"]));
    let reconciler = ExternalEditReconciler::new(backend.clone(), store.clone(), citations);

    let decision = reconciler.reconcile("t1", "page-cited", &key).await?;
    assert!(matches!(decision, Decision::SnapshotWins));

    // Snapshot bytes are untouched — still exactly one row, still "canonical".
    assert_eq!(snapshot_count(&conn, "page-cited").await?, 1);
    let body = snapshot_body(&conn, "page-cited").await?;
    assert_eq!(body, "canonical");
    Ok(())
}

#[tokio::test]
async fn never_snapshotted_page_external_content_becomes_snapshot() -> Result<()> {
    let (_dir, conn, store) = fresh_env()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));

    // No prior CRDT state for "page-new". Just markdown on disk.
    let key = page_key("page-new");
    store
        .write(&key, Bytes::from_static(b"brand new external file"))
        .await?;

    let citations: Arc<dyn CitationLookup> = Arc::new(StubCitations::empty());
    let reconciler = ExternalEditReconciler::new(backend.clone(), store.clone(), citations);

    let decision = reconciler.reconcile("t1", "page-new", &key).await?;
    assert!(matches!(decision, Decision::ExternalWins));

    // Exactly one fresh snapshot row containing the external body.
    assert_eq!(snapshot_count(&conn, "page-new").await?, 1);
    let body = snapshot_body(&conn, "page-new").await?;
    assert_eq!(body, "brand new external file");
    Ok(())
}

#[tokio::test]
async fn no_external_edit_when_no_markdown_changes() -> Result<()> {
    let (_dir, conn, store) = fresh_env()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));

    seed_snapshot(backend.clone(), "page-stable", "stable body").await?;
    let key = page_key("page-stable");
    // On-disk matches snapshot — no external edit.
    store
        .write(&key, Bytes::from_static(b"stable body"))
        .await?;

    let citations: Arc<dyn CitationLookup> = Arc::new(StubCitations::empty());
    let reconciler = ExternalEditReconciler::new(backend.clone(), store.clone(), citations);

    let decision = reconciler.reconcile("t1", "page-stable", &key).await?;
    // Spec: opens that match current snapshot return ExternalWins
    // but as a no-op.
    assert!(matches!(decision, Decision::ExternalWins));

    // No-op: the snapshot row count is unchanged.
    assert_eq!(snapshot_count(&conn, "page-stable").await?, 1);
    let body = snapshot_body(&conn, "page-stable").await?;
    assert_eq!(body, "stable body");
    Ok(())
}

#[tokio::test]
async fn reconciler_uses_trait_for_citation_lookup() -> Result<()> {
    // Toggle the same page id from uncited to cited across two calls
    // and confirm the reconciler's decision flips accordingly. This
    // pins the trait as the only source of "is this page cited?"
    // truth — there is no hidden fallback that bypasses the trait.
    let (_dir, conn, store) = fresh_env()?;
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(conn.clone()));

    seed_snapshot(backend.clone(), "page-flip", "snap").await?;
    let key = page_key("page-flip");
    store
        .write(&key, Bytes::from_static(b"external override"))
        .await?;

    // First pass: uncited → ExternalWins.
    let uncited: Arc<dyn CitationLookup> = Arc::new(StubCitations::empty());
    let r1 = ExternalEditReconciler::new(backend.clone(), store.clone(), uncited);
    assert!(matches!(
        r1.reconcile("t1", "page-flip", &key).await?,
        Decision::ExternalWins
    ));

    // Re-overwrite on disk so the second pass sees an external delta
    // against the snapshot we just wrote.
    store
        .write(&key, Bytes::from_static(b"second external override"))
        .await?;

    // Second pass: cited → SnapshotWins (no new snapshot row).
    let snaps_before = snapshot_count(&conn, "page-flip").await?;
    let cited: Arc<dyn CitationLookup> = Arc::new(StubCitations::with(&["page-flip"]));
    let r2 = ExternalEditReconciler::new(backend.clone(), store.clone(), cited);
    assert!(matches!(
        r2.reconcile("t1", "page-flip", &key).await?,
        Decision::SnapshotWins
    ));
    assert_eq!(snapshot_count(&conn, "page-flip").await?, snaps_before);
    Ok(())
}
