//! Integration tests for the [`escurel_index::Indexer`]
//! audit / rebuild round-trip.
//!
//! Real DuckDB file in a `tempfile::TempDir`, real `FsStore`
//! over a sibling tempdir. No mocks — every assertion is against
//! actual on-disk state.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::{Connection, params};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

/// The three canonical markdown files the round-trip is built on.
const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     ---\n\
     # customer\n\
     \n\
     A customer is the unit of revenue.\n",
);

const INSTANCE_ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Industrial conglomerate. See [[customer::globex-llc]] for the\n\
     comparable.\n",
);

const INSTANCE_GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     ---\n\
     # Globex LLC\n\
     \n\
     Stuttgart-based fintech.\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    duckdb_path: std::path::PathBuf,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().expect("tempdir for store");
    let db_dir = TempDir::new().expect("tempdir for db");
    let duckdb_path = db_dir.path().join("escurel.duckdb");

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).expect("open duckdb");
    Migrator::up(&conn).expect("migrate v1 schema");

    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT)
        .expect("indexer with 768-dim embedder constructs");

    Harness {
        store,
        indexer,
        duckdb_path,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn write_md(store: &Arc<dyn LaneStore>, path: &str, body: &'static str) {
    let key = Key::new(TENANT, path.to_owned()).expect("valid key");
    store
        .write(&key, Bytes::from_static(body.as_bytes()))
        .await
        .expect("write markdown");
}

fn count_pages(path: &std::path::Path) -> i64 {
    let conn = Connection::open(path).expect("open duckdb for count");
    conn.query_row("SELECT count(*) FROM pages", [], |row| row.get(0))
        .expect("count pages")
}

fn page_ids(path: &std::path::Path) -> Vec<String> {
    let conn = Connection::open(path).expect("open duckdb for ids");
    let mut stmt = conn
        .prepare("SELECT page_id FROM pages ORDER BY page_id")
        .expect("prepare");
    let rows: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .expect("query_map")
        .filter_map(std::result::Result::ok)
        .collect();
    rows
}

#[tokio::test]
async fn full_roundtrip_audit_rebuild() {
    let h = fresh_harness();

    // 1. Write three markdown files and index them.
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    write_md(&h.store, INSTANCE_ACME.0, INSTANCE_ACME.1).await;
    write_md(&h.store, INSTANCE_GLOBEX.0, INSTANCE_GLOBEX.1).await;

    for (path, body) in [SKILL_CUSTOMER, INSTANCE_ACME, INSTANCE_GLOBEX] {
        h.indexer
            .update_page(path, body)
            .await
            .unwrap_or_else(|err| panic!("update_page {path}: {err}"));
    }

    let drift = h.indexer.audit().await.expect("audit");
    assert!(drift.is_clean(), "fresh write must audit clean: {drift:?}");
    assert_eq!(count_pages(&h.duckdb_path), 3, "pages table has 3 rows");

    // 2. Delete one markdown file directly from the store.
    let to_delete = Key::new(TENANT, INSTANCE_GLOBEX.0.to_owned()).expect("valid key");
    h.store.delete(&to_delete).await.expect("delete markdown");

    let drift = h.indexer.audit().await.expect("audit after delete");
    assert_eq!(
        drift.indexed_but_no_markdown,
        vec![INSTANCE_GLOBEX.0.to_owned()],
        "audit reports the deleted markdown",
    );
    assert!(
        drift.markdown_not_in_duckdb.is_empty(),
        "no markdown_not_in_duckdb entries: {drift:?}",
    );

    // 3. Drop the DuckDB file entirely (simulates cattle-node loss).
    drop(h);
    let h = fresh_harness();
    // Re-write the surviving markdown to the new tempdir-backed store
    // (the previous tempdir is gone with the harness).
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    write_md(&h.store, INSTANCE_ACME.0, INSTANCE_ACME.1).await;

    h.indexer.rebuild().await.expect("rebuild from markdown");

    let drift = h.indexer.audit().await.expect("audit after rebuild");
    assert!(
        drift.is_clean(),
        "rebuild must reconcile markdown ⟂ duckdb: {drift:?}",
    );
    assert_eq!(
        count_pages(&h.duckdb_path),
        2,
        "pages table has the surviving 2"
    );
    assert_eq!(
        page_ids(&h.duckdb_path),
        vec![INSTANCE_ACME.0.to_owned(), SKILL_CUSTOMER.0.to_owned(),],
    );
}

#[tokio::test]
async fn update_page_writes_one_pages_row_per_call() {
    let h = fresh_harness();
    h.indexer
        .update_page(SKILL_CUSTOMER.0, SKILL_CUSTOMER.1)
        .await
        .expect("update");
    assert_eq!(count_pages(&h.duckdb_path), 1);

    // Idempotent: re-calling update_page for the same page_id
    // upserts in place (no row count change).
    h.indexer
        .update_page(SKILL_CUSTOMER.0, SKILL_CUSTOMER.1)
        .await
        .expect("update again");
    assert_eq!(count_pages(&h.duckdb_path), 1);
}

#[tokio::test]
async fn update_page_persists_to_the_lanestore() {
    // Regression: update_page must write the canonical markdown to the
    // LaneStore (not only the DuckDB index), so `rebuild`/`audit` stay in sync
    // and a dropped/lost index rebuilds the REAL corpus rather than an empty
    // one (crash-recovery contract, docs/spec/storage.md).
    let h = fresh_harness();
    h.indexer
        .update_page(INSTANCE_ACME.0, INSTANCE_ACME.1)
        .await
        .expect("update");

    let key = Key::new(TENANT, INSTANCE_ACME.0.to_owned()).expect("lane key");
    let bytes = h
        .store
        .read(&key)
        .await
        .expect("update_page must persist the page to the LaneStore");
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        INSTANCE_ACME.1,
        "LaneStore content must match what was written"
    );
}

#[tokio::test]
async fn update_page_persists_frontmatter_as_json() {
    let h = fresh_harness();
    h.indexer
        .update_page(INSTANCE_ACME.0, INSTANCE_ACME.1)
        .await
        .expect("update");

    let conn = Connection::open(&h.duckdb_path).expect("open");
    let (skill, page_type, fm_json): (String, String, String) = conn
        .query_row(
            "SELECT skill, page_type, frontmatter::VARCHAR FROM pages WHERE page_id = ?",
            params![INSTANCE_ACME.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("select page row");

    assert_eq!(skill, "customer");
    assert_eq!(page_type, "instance");
    assert!(
        fm_json.contains("\"skill\":\"customer\""),
        "frontmatter JSON must include skill: {fm_json}",
    );
    assert!(
        fm_json.contains("\"id\":\"acme-corp\""),
        "frontmatter JSON must include id: {fm_json}",
    );
}

#[tokio::test]
async fn update_page_persists_wikilinks() {
    let h = fresh_harness();
    h.indexer
        .update_page(INSTANCE_ACME.0, INSTANCE_ACME.1)
        .await
        .expect("update");

    let conn = Connection::open(&h.duckdb_path).expect("open");
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM links \
             WHERE src_page = ? AND link_skill = 'customer' AND dst_page = 'globex-llc'",
            params![INSTANCE_ACME.0],
            |row| row.get(0),
        )
        .expect("count links");
    assert_eq!(
        count, 1,
        "the body's [[customer::globex-llc]] wikilink must land in `links`",
    );
}

#[tokio::test]
async fn audit_flags_markdown_not_in_duckdb() {
    let h = fresh_harness();
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    // Deliberately do NOT call update_page.

    let drift = h.indexer.audit().await.expect("audit");
    assert_eq!(
        drift.markdown_not_in_duckdb,
        vec![SKILL_CUSTOMER.0.to_owned()],
    );
    assert!(drift.indexed_but_no_markdown.is_empty());
}

/// Moving a page must leave nothing behind.
///
/// `delete_page` is a *retraction*: it keeps the markdown as the audit
/// record, which is right when knowledge is withdrawn. A **move** is a
/// different event — the content still exists, at a new id — so retaining
/// the old path is pure noise in the canonical store.
///
/// Restructuring a real tenant's 59 instance ids with update+delete left 59
/// archived husks sitting next to the live pages in the lane, which is what
/// this operation exists to avoid.
#[tokio::test]
async fn move_page_leaves_no_husk_at_the_old_id() {
    let h = fresh_harness();
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    write_md(&h.store, INSTANCE_ACME.0, INSTANCE_ACME.1).await;
    for (path, body) in [SKILL_CUSTOMER, INSTANCE_ACME] {
        h.indexer.update_page(path, body).await.expect("update");
    }

    let dst = "markdown/instances/customer/acme-moved.md";
    assert!(
        h.indexer
            .move_page(INSTANCE_ACME.0, dst)
            .await
            .expect("move_page"),
        "the source existed, so it moved"
    );

    // The old lane file is GONE — not archived, gone. This is the whole
    // difference from delete_page.
    let old_key = Key::new(TENANT, INSTANCE_ACME.0.to_owned()).expect("key");
    assert!(
        h.store.read(&old_key).await.is_err(),
        "no husk may remain at the old id"
    );

    // The content is intact at the new id, and indexed there.
    let new_key = Key::new(TENANT, dst.to_owned()).expect("key");
    let moved = h
        .store
        .read(&new_key)
        .await
        .expect("markdown at the new id");
    assert_eq!(
        std::str::from_utf8(&moved).expect("utf8"),
        INSTANCE_ACME.1,
        "the body is carried across byte for byte"
    );

    // And the audit is clean: no orphan row, no orphan file, no husk.
    let drift = h.indexer.audit().await.expect("audit");
    assert!(drift.is_clean(), "a move leaves no drift: {drift:?}");

    // Survives a from-scratch rebuild — the lane is the source of truth.
    h.indexer.rebuild().await.expect("rebuild");
    let drift = h.indexer.audit().await.expect("audit after rebuild");
    assert!(drift.is_clean(), "still clean after rebuild: {drift:?}");
}

/// Guards against the two ways a move can destroy data.
#[tokio::test]
async fn move_page_refuses_to_overwrite_and_reports_a_missing_source() {
    let h = fresh_harness();
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    write_md(&h.store, INSTANCE_ACME.0, INSTANCE_ACME.1).await;
    write_md(&h.store, INSTANCE_GLOBEX.0, INSTANCE_GLOBEX.1).await;
    for (path, body) in [SKILL_CUSTOMER, INSTANCE_ACME, INSTANCE_GLOBEX] {
        h.indexer.update_page(path, body).await.expect("update");
    }

    // A missing source is not an error, it is "nothing to do" — same shape
    // as delete_page, so a re-run of a migration is free.
    assert!(
        !h.indexer
            .move_page(
                "markdown/instances/customer/nope.md",
                "markdown/instances/customer/x.md"
            )
            .await
            .expect("missing source is Ok(false)"),
    );

    // Moving onto a live page would silently destroy it. Refuse.
    let err = h
        .indexer
        .move_page(INSTANCE_ACME.0, INSTANCE_GLOBEX.0)
        .await
        .expect_err("must not clobber an existing page");
    assert!(
        format!("{err}").contains("exists"),
        "the error names the collision: {err}"
    );
    // ...and the source is untouched by the refusal.
    let src_key = Key::new(TENANT, INSTANCE_ACME.0.to_owned()).expect("key");
    assert!(
        h.store.read(&src_key).await.is_ok(),
        "a refused move leaves the source in place"
    );
}

/// A soft-deleted page must not read as drift.
///
/// `page delete` (#300) retracts a page from the index but retains its
/// markdown in the lane, re-stamped `archived: true`. `rebuild` already
/// honours that and skips archived pages — so the index legitimately has no
/// row for them. `audit` did not, and reported every soft-deleted page under
/// `markdown_not_in_duckdb` for ever.
///
/// The cost was concrete: retiring six pages moved a real tenant from a clean
/// audit to six permanent entries, and "the audit is clean" stopped being a
/// usable health check after any storage-layer change.
#[tokio::test]
async fn audit_ignores_soft_deleted_pages() {
    let h = fresh_harness();
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    write_md(&h.store, INSTANCE_ACME.0, INSTANCE_ACME.1).await;
    for (path, body) in [SKILL_CUSTOMER, INSTANCE_ACME] {
        h.indexer.update_page(path, body).await.expect("update");
    }
    assert!(h.indexer.audit().await.expect("audit").is_clean());

    // Soft-delete the instance: index row goes, markdown stays as the
    // retained audit record.
    assert!(
        h.indexer
            .delete_page(INSTANCE_ACME.0)
            .await
            .expect("delete_page"),
        "the page existed, so it was archived"
    );

    let drift = h.indexer.audit().await.expect("audit after soft delete");
    assert!(
        drift.is_clean(),
        "a retained, archived page is not drift: {drift:?}"
    );

    // The retraction must still survive a from-scratch rebuild, and still
    // not read as drift afterwards.
    h.indexer.rebuild().await.expect("rebuild");
    let drift = h.indexer.audit().await.expect("audit after rebuild");
    assert!(drift.is_clean(), "still clean after rebuild: {drift:?}");
}

/// The guard against over-correcting: a markdown file that is genuinely
/// missing from the index — not archived — must still be reported.
#[tokio::test]
async fn audit_still_flags_unindexed_markdown_that_is_not_archived() {
    let h = fresh_harness();
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    // Deliberately do NOT call update_page.

    let drift = h.indexer.audit().await.expect("audit");
    assert_eq!(
        drift.markdown_not_in_duckdb,
        vec![SKILL_CUSTOMER.0.to_owned()],
        "an unindexed, unarchived page is real drift"
    );
}

#[tokio::test]
async fn rebuild_clears_stale_rows_for_deleted_markdown() {
    // Regression: codex review found rebuild() only upserted current
    // markdown, leaving pages/links/blocks rows for deleted files
    // behind. The admin recovery path (docs/spec/storage.md
    // §Crash recovery) promises rebuild fully reconciles the index
    // to canonical markdown.
    let h = fresh_harness();

    // Seed 3 markdowns and index them.
    write_md(&h.store, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    write_md(&h.store, INSTANCE_ACME.0, INSTANCE_ACME.1).await;
    write_md(&h.store, INSTANCE_GLOBEX.0, INSTANCE_GLOBEX.1).await;
    for (path, body) in [SKILL_CUSTOMER, INSTANCE_ACME, INSTANCE_GLOBEX] {
        h.indexer.update_page(path, body).await.expect("update");
    }
    assert_eq!(count_pages(&h.duckdb_path), 3);

    // Delete one markdown file directly from the store.
    let to_delete = Key::new(TENANT, INSTANCE_GLOBEX.0.to_owned()).expect("valid key");
    h.store.delete(&to_delete).await.expect("delete");

    // Rebuild against the existing (pre-populated) DuckDB.
    h.indexer.rebuild().await.expect("rebuild");

    let drift = h.indexer.audit().await.expect("audit after rebuild");
    assert!(
        drift.is_clean(),
        "rebuild against an existing DuckDB must reconcile to canonical \
         markdown — including dropping stale rows: {drift:?}",
    );
    // audit clean already proves the diff side. Cross-check via the
    // indexer's own connection (not a fresh Connection::open — DuckDB
    // doesn't make concurrent connections see each other's recent
    // commits without a CHECKPOINT).
    let in_db = h
        .indexer
        .audit()
        .await
        .unwrap()
        .markdown_not_in_duckdb
        .len();
    assert_eq!(in_db, 0);
    let listed = h
        .store
        .list(&Key::new(TENANT, "markdown/").expect("k"))
        .await
        .expect("list");
    assert_eq!(listed.len(), 2, "store has the surviving 2 markdown files",);
}

#[tokio::test]
async fn update_page_preserves_wikilink_anchors() {
    // Regression: codex review found the links INSERT was hard-coding
    // dst_anchor = NULL even when parse_wikilinks populated wl.anchor.
    // INSERT OR IGNORE was also collapsing multiple anchors of the
    // same dst into one (since the PK doesn't include dst_anchor).
    let h = fresh_harness();
    let path = "markdown/instances/customer/anchored.md";
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: anchored\n\
                ---\n\
                # Anchored\n\
                \n\
                Two anchors of the same page: [[customer::acme-corp#billing]] \
                and [[customer::acme-corp#renewals]] and one with no anchor: \
                [[customer::globex-llc]].\n";
    h.indexer.update_page(path, body).await.expect("update");

    let conn = Connection::open(&h.duckdb_path).expect("open");

    let billing: i64 = conn
        .query_row(
            "SELECT count(*) FROM links \
             WHERE src_page = ? AND dst_page = 'acme-corp' AND dst_anchor = 'billing'",
            params![path],
            |row| row.get(0),
        )
        .expect("count billing");
    assert_eq!(billing, 1, "anchor `billing` must be preserved as a row");

    let renewals: i64 = conn
        .query_row(
            "SELECT count(*) FROM links \
             WHERE src_page = ? AND dst_page = 'acme-corp' AND dst_anchor = 'renewals'",
            params![path],
            |row| row.get(0),
        )
        .expect("count renewals");
    assert_eq!(
        renewals, 1,
        "anchor `renewals` must be preserved as a distinct row"
    );

    // The bare link records dst_anchor = '' (empty string sentinel —
    // DuckDB PKs forbid NULL columns, so the schema stores '' for
    // "no anchor"; readers project it back to None at the API layer
    // once that layer arrives in M3).
    let bare: i64 = conn
        .query_row(
            "SELECT count(*) FROM links \
             WHERE src_page = ? AND dst_page = 'globex-llc' AND dst_anchor = ''",
            params![path],
            |row| row.get(0),
        )
        .expect("count bare");
    assert_eq!(bare, 1);
}

#[tokio::test]
async fn update_page_populates_dense_vec_with_embedder_output() {
    // M2.1 contract: blocks.dense_vec is NOT NULL after update_page,
    // matches the embedder's output shape, and reflects the embedder's
    // dim() (here 768 from ZeroEmbedder::default).
    let h = fresh_harness();
    h.indexer
        .update_page(SKILL_CUSTOMER.0, SKILL_CUSTOMER.1)
        .await
        .expect("update");

    // Read dense_vec back through the indexer's connection (per the
    // DuckDB second-connection-stale discovered note). We unwrap a
    // length only — the values are zero from ZeroEmbedder.
    let conn = Connection::open(&h.duckdb_path).expect("open");
    let len: i64 = conn
        .query_row(
            "SELECT array_length(dense_vec) FROM blocks WHERE page_id = ?",
            params![SKILL_CUSTOMER.0],
            |row| row.get(0),
        )
        .expect("array_length");
    assert_eq!(
        len, 768,
        "blocks.dense_vec must have the configured 768-dim",
    );

    // And it must not be NULL.
    let is_null: bool = conn
        .query_row(
            "SELECT dense_vec IS NULL FROM blocks WHERE page_id = ?",
            params![SKILL_CUSTOMER.0],
            |row| row.get(0),
        )
        .expect("is_null check");
    assert!(!is_null, "dense_vec must be populated, not NULL");
}

#[tokio::test]
async fn indexer_rejects_embedder_with_wrong_dim() {
    // Schema's blocks.dense_vec is FLOAT[768]; an Indexer built with
    // a different-dim embedder must fail at construction time.
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(384));
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();

    match Indexer::new(store, embedder, conn, TENANT) {
        Ok(_) => panic!("wrong-dim embedder must be rejected"),
        Err(err) => {
            let msg = format!("{err}");
            assert!(
                msg.contains("384"),
                "error must mention the offending dim: {msg}",
            );
        }
    }
}
