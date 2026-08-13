//! Storage-side half of server-stamped attribution (escurel#357, CR-6):
//! the `pages.last_written_by` / `crdt_ops.principal` columns, the migration
//! onto an already-populated database, and survival across `rebuild`.
//!
//! Real DuckDB file, real `Indexer`, real `FsStore`. No mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::{Connection, params};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const ALICE: &str = "consultant:alice";
const NOTE: &str = "markdown/instances/note/n1.md";

const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n---\n# note\n";

fn note_md(body: &str) -> String {
    format!("---\ntype: instance\nskill: note\nid: n1\n---\n# n1\n\n{body}\n")
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = 'main' AND table_name = ? AND column_name = ?",
            params![table, column],
            |row| row.get(0),
        )
        .expect("count columns");
    n > 0
}

/// The migration path that matters: a tenant database provisioned BEFORE
/// #357, already holding rows, gains the attribution columns on the next
/// boot without losing anything.
///
/// The pre-#357 shape is reproduced by dropping the columns from a freshly
/// migrated database — a database that has the rows and not the columns is
/// exactly the state an existing deployment is in.
///
/// This is also the test that pins the migration DECISION: the columns are
/// NULLable. A `NOT NULL` column cannot be added to a populated table, and
/// a `NOT NULL DEFAULT '<something>'` would backfill every historical row
/// with a principal that never wrote it — inventing attribution is worse
/// than admitting there is none.
#[test]
fn an_existing_populated_database_gains_the_attribution_columns_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("escurel.duckdb");
    let conn = Connection::open(&path).expect("open duckdb");
    Migrator::up(&conn).expect("initial migration");

    // Rewind to the pre-#357 schema. DuckDB refuses `DROP COLUMN` while
    // anything depends on the table, so the view and the B-tree indexes come
    // off first and go straight back on — leaving a database that is the
    // pre-#357 shape in full, indexes and dependent view included, which is
    // what `ensure_write_attribution` has to cope with.
    conn.execute_batch(
        "DROP VIEW  IF EXISTS resolved_links; \
         DROP INDEX IF EXISTS pages_slug; \
         DROP INDEX IF EXISTS pages_skill; \
         DROP INDEX IF EXISTS pages_skill_at; \
         DROP INDEX IF EXISTS pages_scenario; \
         DROP INDEX IF EXISTS crdt_ops_page_hlc; \
         ALTER TABLE pages    DROP COLUMN last_written_by; \
         ALTER TABLE crdt_ops DROP COLUMN principal; \
         CREATE INDEX pages_slug         ON pages (slug); \
         CREATE INDEX pages_skill        ON pages (skill); \
         CREATE INDEX pages_skill_at     ON pages (skill, at_ts); \
         CREATE INDEX pages_scenario     ON pages (scenario, skill, at_ts); \
         CREATE INDEX crdt_ops_page_hlc  ON crdt_ops (page_id, hlc);",
    )
    .expect("simulate the pre-357 schema");
    conn.execute(
        "INSERT INTO pages (page_id, slug, skill, page_type, frontmatter, body_hash, \
         created_at, updated_at) \
         VALUES ('old-page', 'old', 'note', 'instance', '{}'::JSON, 'deadbeef', \
                 CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        [],
    )
    .expect("insert a legacy page row");
    conn.execute(
        "INSERT INTO crdt_ops (page_id, op_id, hlc, op_bytes) \
         VALUES ('old-page', 'op-legacy', 1, 'abc'::BLOB)",
        [],
    )
    .expect("insert a legacy op row");

    // The reopen path: `Migrator::up` is NOT re-run against a live database;
    // the idempotent `ensure_*` chain is.
    Migrator::load_extensions(&conn).expect("load extensions");
    Migrator::ensure_provenance_graph(&conn).expect("restore the dependent view");
    // The real ordering hazard: on a reopen the `resolved_links` VIEW
    // already exists and depends on `pages`, so the ADD COLUMN runs
    // underneath it. DuckDB refuses a DROP COLUMN in that situation; if it
    // refused ADD COLUMN too, every existing tenant would fail to boot.
    Migrator::ensure_write_attribution(&conn).expect("attribution migration on a populated db");

    assert!(
        column_exists(&conn, "pages", "last_written_by"),
        "pages.last_written_by must be added on reopen"
    );
    assert!(
        column_exists(&conn, "crdt_ops", "principal"),
        "crdt_ops.principal must be added on reopen"
    );

    // Nothing was dropped, and the historical rows read NULL rather than a
    // fabricated principal.
    let (page_count, page_principal): (i64, Option<String>) = conn
        .query_row(
            "SELECT count(*), any_value(last_written_by) FROM pages WHERE page_id = 'old-page'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read back the legacy page row");
    assert_eq!(page_count, 1, "the legacy page row must survive");
    assert_eq!(
        page_principal, None,
        "a row written before attribution existed must read NULL, not a guess"
    );

    let (op_count, op_principal): (i64, Option<String>) = conn
        .query_row(
            "SELECT count(*), any_value(principal) FROM crdt_ops WHERE op_id = 'op-legacy'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read back the legacy op row");
    assert_eq!(op_count, 1, "the legacy op row must survive");
    assert_eq!(op_principal, None, "a legacy op must read NULL");

    // Idempotent: running it again on the now-migrated database is a no-op.
    Migrator::ensure_write_attribution(&conn).expect("second run is a no-op");

    // THE TRAP. `crdt_ops.applied_at` is `DEFAULT CURRENT_TIMESTAMP`, and
    // DuckDB cannot replay an `ALTER TABLE … ADD COLUMN` from the WAL against
    // a table with a function-valued default — replay re-binds the defaults
    // and dies with "Calling DatabaseManager::GetDefaultDatabase with no
    // default database set". The migrating process keeps working; the NEXT
    // process to open the file cannot start. `ensure_write_attribution`
    // checkpoints for exactly this reason.
    //
    // The assertion is a second open of the same file while the first
    // connection is still alive, which is what the boot path does
    // (`try_clone` for the CRDT backend) and what every reopen does.
    Connection::open(&path).expect(
        "a second connection must be able to open the migrated file — \
         an un-checkpointed ADD COLUMN on `crdt_ops` leaves an unreplayable WAL",
    );
}

/// `rebuild` drops and re-derives `pages` from the markdown lane. The lane
/// holds no attribution, so a naive rebuild would silently erase the audit
/// trail of every page — the one operation an operator runs precisely when
/// things have gone wrong.
///
/// The positive control is the *other* page: one written with no principal
/// stays NULL, so the test cannot pass by stamping everything.
#[tokio::test]
async fn rebuild_preserves_the_recorded_writer() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();

    let skill_path = "markdown/skills/note.md";
    for (path, body) in [(skill_path, NOTE_SKILL), (NOTE, &note_md("hello"))] {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        store
            .write(&key, Bytes::from(body.to_owned()))
            .await
            .unwrap();
    }
    indexer.update_page(skill_path, NOTE_SKILL).await.unwrap();
    indexer
        .update_page_as(NOTE, &note_md("hello"), Some(ALICE))
        .await
        .unwrap();

    let before = indexer.expand(NOTE, None, None).await.unwrap().unwrap();
    assert_eq!(
        before.last_written_by.as_deref(),
        Some(ALICE),
        "precondition: the write is attributed"
    );

    indexer.rebuild().await.unwrap();

    let after = indexer.expand(NOTE, None, None).await.unwrap().unwrap();
    assert_eq!(
        after.last_written_by.as_deref(),
        Some(ALICE),
        "rebuild must not erase attribution — the lane cannot re-derive it"
    );

    // Positive control: an unattributed page stays unattributed.
    let skill = indexer
        .expand(skill_path, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        skill.last_written_by, None,
        "a page written with no principal must not gain one"
    );
}
