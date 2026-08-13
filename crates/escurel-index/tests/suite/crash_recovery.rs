//! Failure-injection / crash-recovery-matrix integration tests.
//!
//! These cover the recovery matrix from
//! `docs/spec/storage.md §Crash recovery summary`, exercised against
//! the *real* components: a real DuckDB file in a `tempfile::TempDir`,
//! a real [`FsStore`] over a sibling tempdir, and real markdown
//! round-trips through [`Indexer::update_page`] / [`Indexer::rebuild`].
//! No mocks — every assertion is against actual on-disk state.
//!
//! Matrix rows covered here (others live alongside their primitives —
//! see `index_roundtrip.rs` for the audit/rebuild diff cases and
//! `escurel-crdt/tests/reconciler_roundtrip.rs` for the
//! markdown-newer-than-snapshot reconciliation):
//!
//! - **Process killed mid-write** rolls the DuckDB transaction back so
//!   `pages` / `links` / `blocks` / `crdt_ops` all revert together
//!   (`mid_write_panic_rolls_back_all_tables`).
//! - **Cattle node destroyed; `escurel.duckdb` gone; markdown intact**
//!   → automatic `rebuild` from canonical markdown reproduces the
//!   pre-loss index, byte-for-byte where the spec promises it
//!   (`node_loss_then_rebuild_from_markdown_reproduces_index`,
//!   `node_loss_then_rebuild_audit_is_clean`).
//! - The DuckDB **second-connection-stale** guardrail
//!   (`docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md`):
//!   reads through the writer's own connection see its committed
//!   writes (`shared_connection_sees_writes_second_connection_would_miss`).

use std::sync::Arc;

use bytes::Bytes;
use duckdb::{Connection, params};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

/// Canonical markdown the recovery scenarios are built on: one skill
/// page and two instances, one of which cites the other.
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

const ALL_MARKDOWN: [(&str, &str); 3] = [SKILL_CUSTOMER, INSTANCE_ACME, INSTANCE_GLOBEX];

/// A real `FsStore` + real DuckDB file, kept alive together. The
/// tempdirs are owned so they outlive the indexer; dropping the
/// harness destroys both (used to simulate cattle-node loss).
struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    duckdb_path: std::path::PathBuf,
    store_dir: TempDir,
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
        store_dir,
        _db_dir: db_dir,
    }
}

/// Build a harness whose `FsStore` is backed by `store_dir` (so the
/// canonical markdown can survive across a simulated DuckDB-file
/// loss) but whose DuckDB file lives in a fresh tempdir.
fn harness_on_store(store_dir: TempDir) -> Harness {
    let db_dir = TempDir::new().expect("tempdir for db");
    let duckdb_path = db_dir.path().join("escurel.duckdb");

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).expect("open duckdb");
    Migrator::up(&conn).expect("migrate v1 schema");

    let indexer =
        Indexer::new(Arc::clone(&store), embedder, conn, TENANT).expect("indexer constructs");

    Harness {
        store,
        indexer,
        duckdb_path,
        store_dir,
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

/// Seed every canonical markdown file on the store and index it.
async fn seed_tenant(h: &Harness) {
    for (path, body) in ALL_MARKDOWN {
        write_md(&h.store, path, body).await;
    }
    for (path, body) in ALL_MARKDOWN {
        h.indexer
            .update_page(path, body)
            .await
            .unwrap_or_else(|err| panic!("update_page {path}: {err}"));
    }
}

/// Row counts of the four tables the spec's "process killed mid-write"
/// row promises revert together. Reads through the connection it is
/// handed — `mid_write_panic_rolls_back_all_tables` owns its single
/// connection (the sole accessor of the file), so this never trips the
/// second-connection-stale trap.
fn table_counts(conn: &Connection) -> (i64, i64, i64, i64) {
    let pages: i64 = conn
        .query_row("SELECT count(*) FROM pages", [], |r| r.get(0))
        .expect("count pages");
    let links: i64 = conn
        .query_row("SELECT count(*) FROM links", [], |r| r.get(0))
        .expect("count links");
    let blocks: i64 = conn
        .query_row("SELECT count(*) FROM blocks", [], |r| r.get(0))
        .expect("count blocks");
    let crdt_ops: i64 = conn
        .query_row("SELECT count(*) FROM crdt_ops", [], |r| r.get(0))
        .expect("count crdt_ops");
    (pages, links, blocks, crdt_ops)
}

/// The observable, index-served state of the seeded tenant, gathered
/// entirely through the [`Indexer`]'s *own* read APIs (the writer's
/// connection) — never a second `Connection::open`, per
/// `docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md`.
/// Comparing this struct before node loss and after rebuild proves the
/// index was reproduced.
#[derive(Debug, PartialEq)]
struct ObservableState {
    skills: Vec<escurel_index::SkillInfo>,
    instances: Vec<escurel_index::InstanceInfo>,
    /// Outbound edges from the acme-corp instance (the only seeded
    /// wikilink: acme-corp → globex-llc).
    acme_out_edges: Vec<escurel_index::Edge>,
    /// `(page_id, block count, joined block body)` for each page, so a
    /// missing or extra searchable block shows up as a diff.
    page_blocks: Vec<(String, usize, String)>,
}

async fn observe(indexer: &Indexer) -> ObservableState {
    use escurel_index::Direction;

    let skills = indexer.list_skills().await.expect("list_skills");
    let instances = indexer
        .list_instances("customer", None, None, None, None, None)
        .await
        .expect("list_instances");
    let acme_out_edges = indexer
        .neighbours(INSTANCE_ACME.0, Direction::Out, None, None, None)
        .await
        .expect("neighbours out");

    // Block inventory per page, via expand (index-served from blocks).
    let mut page_blocks = Vec::new();
    for (path, _) in ALL_MARKDOWN {
        if let Some(expanded) = indexer.expand(path, None, None).await.expect("expand") {
            let bodies = expanded
                .blocks
                .iter()
                .map(|b| b.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            page_blocks.push((path.to_owned(), expanded.blocks.len(), bodies));
        }
    }
    page_blocks.sort();

    ObservableState {
        skills,
        instances,
        acme_out_edges,
        page_blocks,
    }
}

/// Spec `docs/spec/storage.md §Crash recovery summary`, row
/// "Process killed mid-write": *DuckDB rolls back the transaction;
/// pages, links, blocks (with vss/fts updates), crdt_ops all revert
/// together.* We exercise the real v1 schema (real HNSW + FTS
/// indexes, real `crdt_ops` table) on a real on-disk DuckDB file: a
/// write transaction that mutates all four tables is abandoned
/// without commit — exactly what an OS-level kill leaves behind — and
/// the next read must observe the prior committed state intact, with
/// zero partial rows from the aborted transaction.
#[test]
fn mid_write_panic_rolls_back_all_tables() {
    let db_dir = TempDir::new().expect("tempdir for db");
    let duckdb_path = db_dir.path().join("escurel.duckdb");

    // One connection is the sole accessor of the file for the whole
    // test (per the second-connection-stale note), so every read sees
    // the writer's committed state via in-process MVCC.
    let mut conn = Connection::open(&duckdb_path).expect("open duckdb");
    Migrator::up(&conn).expect("migrate v1 schema");

    // Commit a baseline page across all four tables.
    {
        let tx = conn.transaction().expect("begin baseline tx");
        insert_full_page(&tx, "markdown/skills/customer.md", "customer", "skill");
        tx.commit().expect("commit baseline");
    }
    let baseline = table_counts(&conn);
    assert_eq!(
        baseline,
        (1, 1, 1, 1),
        "baseline seeds exactly one row in each of pages/links/blocks/crdt_ops",
    );

    // Begin a second transaction that mutates ALL FOUR tables for a
    // *new* page, then abandon it without committing — the faithful
    // analogue of a SIGKILL after parse but before `tx.commit()` in
    // `Indexer::update_page`. DuckDB rolls the whole transaction back
    // when the (un-committed) `Transaction` is dropped.
    {
        let tx = conn.transaction().expect("begin doomed tx");
        insert_full_page(
            &tx,
            "markdown/instances/customer/acme-corp.md",
            "customer",
            "instance",
        );
        // Sanity: inside the open transaction the new rows ARE visible
        // to this same transaction — proving we genuinely wrote them,
        // so the post-rollback emptiness is a real rollback and not a
        // no-op write.
        let in_tx: i64 = tx
            .query_row("SELECT count(*) FROM pages", [], |r| r.get(0))
            .expect("count inside tx");
        assert_eq!(in_tx, 2, "the doomed tx really did insert the second page");
        // Drop without commit == process killed mid-write.
        drop(tx);
    }

    // After the abandoned transaction, every table is back to the
    // baseline: pages/links/blocks/crdt_ops reverted together, with no
    // partial state from the doomed write surviving.
    let after = table_counts(&conn);
    assert_eq!(
        after, baseline,
        "an abandoned (uncommitted) write transaction must roll back \
         pages/links/blocks/crdt_ops together: got {after:?}, want {baseline:?}",
    );

    // And the surviving page is exactly the committed baseline.
    let surviving: String = conn
        .query_row("SELECT page_id FROM pages", [], |r| r.get(0))
        .expect("select surviving page");
    assert_eq!(surviving, "markdown/skills/customer.md");
}

/// Insert one page's worth of rows across pages/links/blocks/crdt_ops,
/// mirroring the table shapes `Indexer::update_page` writes (plus a
/// `crdt_ops` row, which `update_page` does not write today but the
/// spec's atomicity claim explicitly covers). Used only to construct
/// a multi-table write transaction whose all-or-nothing rollback we
/// then assert.
fn insert_full_page(tx: &duckdb::Transaction<'_>, page_id: &str, skill: &str, page_type: &str) {
    tx.execute(
        "INSERT INTO pages \
         (page_id, slug, skill, page_type, frontmatter, body_hash, created_at, updated_at) \
         VALUES (?, ?, ?, ?, '{}'::JSON, 'deadbeef', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        params![page_id, page_id, skill, page_type],
    )
    .expect("insert page");

    tx.execute(
        "INSERT INTO links \
         (src_page, src_anchor, src_field, dst_page, dst_anchor, link_skill) \
         VALUES (?, '', NULL, 'globex-llc', '', ?)",
        params![page_id, skill],
    )
    .expect("insert link");

    // blocks carries the HNSW-indexed dense_vec; a zero vector keeps
    // the literal small but still exercises the vss index write path.
    let zeros = format!("[{}]", vec!["0"; 768].join(","));
    let block_sql = format!(
        "INSERT INTO blocks \
         (block_id, page_id, anchor, ordinal, body, dense_vec, skill, page_type) \
         VALUES (?, ?, 'blk-0', 0, 'body', {zeros}::FLOAT[768], ?, ?)",
    );
    tx.execute(
        &block_sql,
        params![format!("{page_id}:blk-0"), page_id, skill, page_type],
    )
    .expect("insert block");

    tx.execute(
        "INSERT INTO crdt_ops (page_id, op_id, hlc, op_bytes) VALUES (?, 'op-0', 1, ?)",
        params![page_id, [0u8, 1, 2, 3].as_slice()],
    )
    .expect("insert crdt_op");
}

/// Spec `docs/spec/storage.md §HNSW persistence model` /
/// §Crash-recovery row "Cattle node destroyed; `escurel.duckdb` gone;
/// markdown intact on LaneStore": the DuckDB file is a *rebuildable
/// derivative* of the canonical markdown. We seed a tenant, simulate
/// node loss by deleting the entire `escurel.duckdb` file, then on a
/// fresh DuckDB run `Migrator::up` + `rebuild` from the surviving
/// markdown and assert the rebuilt index reproduces the pre-loss
/// pages, links, and searchable blocks.
#[tokio::test]
async fn node_loss_then_rebuild_from_markdown_reproduces_index() {
    let h = fresh_harness();
    seed_tenant(&h).await;

    // Capture the pre-loss observable state via the indexer's own
    // read APIs (writer connection, never a second open).
    let before = observe(&h.indexer).await;
    assert_eq!(before.skills.len(), 1, "one skill seeded");
    assert_eq!(before.instances.len(), 2, "two instances seeded");
    assert_eq!(
        before.acme_out_edges.len(),
        1,
        "one wikilink seeded (acme-corp → globex-llc)",
    );
    assert_eq!(
        before.page_blocks.len(),
        3,
        "three pages, each with a block"
    );
    assert!(
        before.page_blocks.iter().all(|(_, n, _)| *n == 1),
        "one searchable block per page",
    );

    // --- Cattle-node loss. Keep the store tempdir (canonical
    // markdown survives on the LaneStore); destroy everything else,
    // including the DuckDB file. ---
    let store_dir = h.store_dir; // moved out before drop
    let duckdb_path = h.duckdb_path.clone();
    drop(h.indexer); // releases the connection / file handle
    std::fs::remove_file(&duckdb_path).expect("delete escurel.duckdb (simulate node loss)");
    assert!(
        !duckdb_path.exists(),
        "the DuckDB file is gone, the cattle-node-loss precondition",
    );

    // --- Fresh allocation on a new host: same canonical markdown,
    // brand-new empty DuckDB. The server's first-request path runs
    // Migrator::up then rebuild(tenant). ---
    let h2 = harness_on_store(store_dir);
    // Confirm the markdown genuinely survived and the new DuckDB is
    // empty before the rebuild.
    let surviving_md = h2
        .store
        .list(&Key::new(TENANT, "markdown/").expect("k"))
        .await
        .expect("list surviving markdown");
    assert_eq!(
        surviving_md.len(),
        3,
        "all 3 markdown files survived node loss",
    );
    assert!(
        h2.indexer
            .list_skills()
            .await
            .expect("list_skills")
            .is_empty(),
        "the fresh DuckDB starts with no pages",
    );

    h2.indexer.rebuild().await.expect("rebuild from markdown");

    // The rebuilt index reproduces the pre-loss observable state in
    // full: same skills, same instances (incl. frontmatter), same
    // typed link, same searchable blocks.
    let after = observe(&h2.indexer).await;
    assert_eq!(
        after, before,
        "rebuild from canonical markdown must reproduce the pre-loss index",
    );
}

/// Companion to the reproduction test: after the cattle-node-loss
/// rebuild, `audit()` reports clean drift in both directions — every
/// surviving markdown file is indexed and no index row lacks backing
/// markdown. This is the spec's promise that the two recovery
/// primitives (`audit`, `rebuild`) leave the index fully reconciled.
#[tokio::test]
async fn node_loss_then_rebuild_audit_is_clean() {
    let h = fresh_harness();
    seed_tenant(&h).await;

    let drift = h.indexer.audit().await.expect("audit before loss");
    assert!(drift.is_clean(), "seeded tenant audits clean: {drift:?}");

    // Simulate node loss: drop the indexer, delete the DuckDB file,
    // keep the markdown on the store.
    let store_dir = h.store_dir;
    drop(h.indexer);
    std::fs::remove_file(&h.duckdb_path).expect("delete escurel.duckdb");

    // Fresh DuckDB on the surviving store; rebuild.
    let h2 = harness_on_store(store_dir);

    // Pre-rebuild, audit sees every markdown file as not-yet-indexed.
    let drift = h2.indexer.audit().await.expect("audit before rebuild");
    assert_eq!(
        drift.markdown_not_in_duckdb.len(),
        3,
        "fresh DuckDB: all surviving markdown is not-yet-indexed: {drift:?}",
    );
    assert!(
        drift.indexed_but_no_markdown.is_empty(),
        "fresh DuckDB has no orphan index rows: {drift:?}",
    );

    h2.indexer.rebuild().await.expect("rebuild from markdown");

    let drift = h2.indexer.audit().await.expect("audit after rebuild");
    assert!(
        drift.is_clean(),
        "rebuild from canonical markdown must leave audit clean: {drift:?}",
    );
}

/// Regression pinning the safe DuckDB access pattern from
/// `docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md`:
/// reads MUST go through the writer's own connection so they observe
/// its committed writes via in-process MVCC. A *separate*
/// `Connection::open` to the same file may return a stale snapshot
/// after a burst of write transactions (the trap the note documents).
///
/// We positively assert the safe pattern — that the indexer's own
/// read APIs (`list_skills`, `list_instances`, `audit`, `expand`) see
/// writes committed by `update_page` immediately, across a burst of
/// write transactions like the one that originally surfaced the trap.
/// The second-connection failure mode is intentionally not
/// re-triggered (it is non-deterministic by nature); this test guards
/// that the production read path keeps reads on the writer's
/// connection so the trap can never silently return.
#[tokio::test]
async fn shared_connection_sees_writes_second_connection_would_miss() {
    let h = fresh_harness();
    // Pre-populate, then issue a burst of write transactions (the
    // DELETE…DELETE…INSERT…INSERT pattern in the discovered note that
    // made a second connection's snapshot lag). Each update_page is a
    // full write transaction touching pages/links/blocks.
    for (path, body) in ALL_MARKDOWN {
        write_md(&h.store, path, body).await;
    }
    for _ in 0..3 {
        for (path, body) in ALL_MARKDOWN {
            h.indexer
                .update_page(path, body)
                .await
                .expect("update_page");
        }
    }

    // Reads through the indexer's OWN connection see every committed
    // write immediately — the safe pattern the note mandates. If the
    // production read path ever regressed to opening a fresh
    // `Connection::open` per read, this is where the stale-snapshot
    // trap would re-surface as a flaky / short count.
    let skills = h.indexer.list_skills().await.expect("list_skills");
    assert_eq!(skills.len(), 1, "writer-connection read sees the skill");

    let instances = h
        .indexer
        .list_instances("customer", None, None, None, None, None)
        .await
        .expect("list_instances");
    assert_eq!(
        instances.len(),
        2,
        "writer-connection read sees both instances after a write burst",
    );

    // expand/neighbours (which also read via the writer connection)
    // see the freshly committed blocks and links.
    let acme = h
        .indexer
        .expand(INSTANCE_ACME.0, None, None)
        .await
        .expect("expand")
        .expect("acme page present");
    assert_eq!(
        acme.blocks.len(),
        1,
        "writer-connection expand sees the committed block",
    );
    assert_eq!(
        acme.wikilinks_out.len(),
        1,
        "writer-connection expand sees the committed wikilink",
    );

    let drift = h.indexer.audit().await.expect("audit");
    assert!(
        drift.is_clean(),
        "audit (writer-connection read) is clean after the burst: {drift:?}",
    );
}
