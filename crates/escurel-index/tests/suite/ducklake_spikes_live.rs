//! Phase-0 spikes for moving append-shaped data (chat, events) into
//! DuckLake tables. Both questions are load-bearing for that design and
//! neither is answered anywhere in the repo today:
//!
//! **S1 — can N processes ATTACH the same lake READ-WRITE?**
//! The original spike (`docs/notes/discovered/2026-07-17-ducklake-spike-results.md`)
//! only ever tested ONE read-write attacher plus `READ_ONLY` readers, and
//! ADR-0009 says multi-writer conflict was "avoided entirely" — so the
//! behaviour is uncharacterised. It decides whether reader replicas can
//! append directly to a lake-backed chat/events table, or whether every
//! append has to be forwarded to the single writer process.
//!
//! **S2 — does `ducklake_merge_adjacent_files` compact append-shaped tables?**
//! With `DATA_INLINING_ROW_LIMIT 0` (mandatory — inlining stores rows in the
//! catalog, which is the residency problem we are trying to escape) every
//! single-row INSERT writes its own Parquet file. Compaction is named as a
//! follow-up in ADR-0009:81-83 and has never been exercised.
//!
//! Opt-in (needs Docker): `cargo test -p escurel-index --features
//! live-ducklake --test ducklake_spikes_live -- --nocapture`.

#![cfg(feature = "live-ducklake")]

use duckdb::Connection;
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, attach_sql, install_load_sql, secret_sql,
};
use escurel_storage::{S3Store, S3StoreConfig};
use testcontainers_modules::minio::MinIO;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const BUCKET: &str = "escurel-spike";

struct Live {
    cfg: LakeConfig,
    _pg: ContainerAsync<Postgres>,
    _minio: ContainerAsync<MinIO>,
}

async fn live() -> Live {
    let pg = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn =
        format!("host=127.0.0.1 port={pg_port} user=postgres password=postgres dbname=postgres");

    let minio = MinIO::default().start().await.expect("start minio");
    let s3_port = minio.get_host_port_ipv4(9000).await.expect("minio port");
    let s3 = S3Store::new(S3StoreConfig {
        bucket: BUCKET.to_owned(),
        prefix: "unused".to_owned(),
        endpoint_url: format!("http://127.0.0.1:{s3_port}"),
        region: "us-east-1".to_owned(),
        access_key_id: "minioadmin".to_owned(),
        secret_access_key: "minioadmin".to_owned(),
    })
    .await
    .expect("build S3Store");
    s3.ensure_bucket().await.expect("create bucket");

    Live {
        cfg: LakeConfig {
            catalog_dsn: dsn,
            data_path: format!("s3://{BUCKET}/data/"),
            object_store: ObjectStoreSecret::S3 {
                endpoint: format!("127.0.0.1:{s3_port}"),
                access_key_id: "minioadmin".to_owned(),
                secret_access_key: "minioadmin".to_owned(),
                region: "us-east-1".to_owned(),
                use_ssl: false,
            },
        },
        _pg: pg,
        _minio: minio,
    }
}

/// A connection with the lake attached. `read_only = false` gives the
/// writer shape (inlining disabled, per `attach_sql`).
fn conn_for(cfg: &LakeConfig, read_only: bool) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&install_load_sql(cfg)).unwrap();
    if let Some(sql) = secret_sql(cfg).unwrap() {
        conn.execute_batch(&sql).unwrap();
    }
    conn.execute_batch(&attach_sql(cfg, read_only).unwrap())
        .unwrap();
    conn
}

fn count_rows(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM lake.appends", [], |r| r.get(0))
        .unwrap()
}

/// Number of Parquet files actually on the object store. Globbing the data
/// path is the unambiguous measure — `ducklake_table_info` returns one row
/// per TABLE, not per file, so counting it reports 1 no matter how many
/// files exist (which is exactly the mistake that made the first run of
/// this spike look like compaction was a no-op).
fn count_data_files(conn: &Connection) -> i64 {
    conn.query_row(
        &format!("SELECT count(*) FROM glob('s3://{BUCKET}/data/**/*.parquet')"),
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(-1)
}

// --- S1: concurrent read-write ATTACH ---------------------------------

#[tokio::test]
async fn s1_two_readwrite_attaches_to_one_lake() {
    let live = live().await;

    let a = conn_for(&live.cfg, false);
    a.execute_batch("CREATE TABLE IF NOT EXISTS lake.appends (who VARCHAR, n INTEGER);")
        .unwrap();

    // The actual question: does a SECOND read-write attach even succeed?
    let b = conn_for(&live.cfg, false);

    const N: i32 = 50;
    let mut a_errs = 0;
    let mut b_errs = 0;
    for i in 0..N {
        if a.execute("INSERT INTO lake.appends VALUES ('a', ?)", [i])
            .is_err()
        {
            a_errs += 1;
        }
        if b.execute("INSERT INTO lake.appends VALUES ('b', ?)", [i])
            .is_err()
        {
            b_errs += 1;
        }
    }

    let total = count_rows(&a);
    let a_rows: i64 = a
        .query_row(
            "SELECT count(*) FROM lake.appends WHERE who = 'a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let b_rows: i64 = a
        .query_row(
            "SELECT count(*) FROM lake.appends WHERE who = 'b'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    println!(
        "S1 RESULT: attached=2 rw, inserts={N}+{N}, errors a={a_errs} b={b_errs}, \
         rows total={total} a={a_rows} b={b_rows}, data_files={}",
        count_data_files(&a),
    );

    // Not asserting success — this spike exists to CHARACTERISE the
    // behaviour, and a failure here is a legitimate, informative result
    // that decides the design. What must not happen silently is lost
    // writes: every insert that reported success must be readable.
    let expected = i64::from(N) * 2 - i64::from(a_errs) - i64::from(b_errs);
    assert_eq!(
        total, expected,
        "every INSERT that returned Ok must be durable — \
         {expected} acknowledged, {total} readable (silent write loss)",
    );
}

/// A reader attached READ_ONLY must observe a concurrent writer's committed
/// rows — the property a lake-backed chat surface would depend on for
/// cross-replica reads.
#[tokio::test]
async fn s1_readonly_reader_sees_committed_appends() {
    let live = live().await;
    let w = conn_for(&live.cfg, false);
    w.execute_batch("CREATE TABLE IF NOT EXISTS lake.appends (who VARCHAR, n INTEGER);")
        .unwrap();
    w.execute("INSERT INTO lake.appends VALUES ('w', 1)", [])
        .unwrap();

    let r = conn_for(&live.cfg, true);
    let seen = count_rows(&r);
    println!("S1b RESULT: reader saw {seen} row(s) after a committed write");
    assert!(
        seen >= 1,
        "a READ_ONLY attach opened after the commit must see it",
    );
}

// --- S2: compaction of an append-shaped table -------------------------

#[tokio::test]
async fn s2_compaction_of_single_row_appends() {
    let live = live().await;
    let w = conn_for(&live.cfg, false);
    w.execute_batch("CREATE TABLE IF NOT EXISTS lake.appends (who VARCHAR, n INTEGER);")
        .unwrap();

    // Single-statement autocommits — exactly the shape `append_message` /
    // `capture_event` produce, and the shape that makes inlining-off
    // expensive.
    const N: i32 = 200;
    for i in 0..N {
        w.execute("INSERT INTO lake.appends VALUES ('w', ?)", [i])
            .unwrap();
    }
    let before = count_data_files(&w);
    let rows_before = count_rows(&w);

    let merge = w.execute_batch("CALL ducklake_merge_adjacent_files('lake');");
    let merge_err = merge.as_ref().err().map(std::string::ToString::to_string);
    let after = count_data_files(&w);
    let rows_after = count_rows(&w);

    println!(
        "S2 RESULT: {N} single-row appends → data_files before={before} after={after}, \
         rows before={rows_before} after={rows_after}, merge_err={merge_err:?}",
    );

    assert_eq!(
        rows_before, rows_after,
        "compaction must not lose or duplicate rows",
    );
    assert_eq!(
        rows_after,
        i64::from(N),
        "all appends must survive compaction",
    );

    // One Parquet file per single-row append, as inlining-off implies.
    assert_eq!(
        before,
        i64::from(N),
        "with DATA_INLINING_ROW_LIMIT 0 each autocommitted INSERT writes \
         its own Parquet file",
    );

    // Pin the CURRENT (bad) behaviour so an upgrade that fixes it is
    // loud rather than silent: compaction does not reduce the file count.
    // If this assertion ever fails, ducklake learned to compact
    // append-shaped tables — revisit the chat/events storage decision,
    // which is constrained by exactly this.
    assert!(
        after >= before,
        "compaction is expected to be a NO-OP on this ducklake build \
         (before={before}, after={after}); if it now compacts, the \
         one-file-per-append constraint is lifted and the design that \
         worked around it should be reconsidered",
    );
}

/// What compaction/maintenance entry points does this ducklake build
/// actually expose? `ducklake_merge_adjacent_files` is the one ADR-0009
/// names, but it is a silent no-op on an append-shaped table, so the
/// question is whether anything else does the job.
#[tokio::test]
async fn s2b_available_ducklake_maintenance_functions() {
    let live = live().await;
    let w = conn_for(&live.cfg, false);
    let mut stmt = w
        .prepare(
            "SELECT function_name FROM duckdb_functions() \
             WHERE function_name ILIKE '%ducklake%' \
             AND (function_name ILIKE '%merge%' OR function_name ILIKE '%compact%' \
                  OR function_name ILIKE '%rewrite%' OR function_name ILIKE '%flush%' \
                  OR function_name ILIKE '%expire%' OR function_name ILIKE '%cleanup%') \
             ORDER BY 1",
        )
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .collect();
    println!("S2b RESULT: maintenance functions = {names:?}");
}

/// Try every compaction entry point + overload against an append-shaped
/// table. `ducklake_merge_adjacent_files('lake')` is a silent no-op (S2),
/// so the design needs to know whether ANY of these actually collapses
/// one-file-per-append.
#[tokio::test]
async fn s2c_which_compaction_call_actually_compacts() {
    let live = live().await;
    let w = conn_for(&live.cfg, false);
    w.execute_batch("CREATE TABLE IF NOT EXISTS lake.appends (who VARCHAR, n INTEGER);")
        .unwrap();
    const N: i32 = 100;
    for i in 0..N {
        w.execute("INSERT INTO lake.appends VALUES ('w', ?)", [i])
            .unwrap();
    }
    let before = count_data_files(&w);

    for call in [
        "CALL ducklake_merge_adjacent_files('lake');",
        "CALL ducklake_merge_adjacent_files('lake', 'appends');",
        "CALL ducklake_rewrite_data_files('lake');",
        "CALL ducklake_rewrite_data_files('lake', 'appends');",
    ] {
        let res = w.execute_batch(call);
        let files = count_data_files(&w);
        let rows = count_rows(&w);
        println!(
            "S2c: {call:<55} -> files={files} rows={rows} err={:?}",
            res.err().map(|e| e.to_string()),
        );
    }
    println!("S2c RESULT: started at files={before}, rows={N}");
}

/// S3 — can ONE connection attach the same lake twice under two aliases,
/// one READ_ONLY (the corpus, as `adopt_lake` needs) and one read-write
/// (an append-shaped chat/events table)? If yes, a lake-backed chat
/// surface is a second attach alongside the existing one, mirroring how
/// `chat_pg`/`events_pg`/`crdt_pg` sit beside the corpus. If no, the
/// reader's corpus attach itself has to become read-write.
#[tokio::test]
async fn s3_same_lake_attached_twice_on_one_connection() {
    let live = live().await;

    // Seed a table via a plain writer first.
    let w = conn_for(&live.cfg, false);
    w.execute_batch("CREATE TABLE IF NOT EXISTS lake.appends (who VARCHAR, n INTEGER);")
        .unwrap();
    w.execute("INSERT INTO lake.appends VALUES ('seed', 0)", [])
        .unwrap();

    // Fresh connection: attach READ_ONLY as `lake` (corpus shape), then
    // attach the SAME lake read-write under a second alias.
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(&install_load_sql(&live.cfg)).unwrap();
    if let Some(sql) = secret_sql(&live.cfg).unwrap() {
        c.execute_batch(&sql).unwrap();
    }
    c.execute_batch(&attach_sql(&live.cfg, true).unwrap())
        .unwrap();

    let second = attach_sql(&live.cfg, false)
        .unwrap()
        .replace(" AS lake ", " AS lake_rw ");
    let attach_res = c.execute_batch(&second);
    let attach_err = attach_res
        .as_ref()
        .err()
        .map(std::string::ToString::to_string);

    let wrote = if attach_err.is_none() {
        c.execute("INSERT INTO lake_rw.appends VALUES ('second', 1)", [])
            .map(|n| n.to_string())
            .unwrap_or_else(|e| format!("ERR: {e}"))
    } else {
        "skipped".to_owned()
    };
    let readonly_sees: i64 = c
        .query_row("SELECT count(*) FROM lake.appends", [], |r| r.get(0))
        .unwrap_or(-1);

    println!(
        "S3 RESULT: second attach err={attach_err:?}, rw insert={wrote}, \
         read_only alias sees {readonly_sees} row(s)",
    );
}

/// S4 — since no built-in compaction call works (S2), can we compact the
/// way `publish_lake` already does: `CREATE OR REPLACE TABLE t AS SELECT
/// * FROM t`? That is the proven pattern in this codebase for the corpus
/// tables, and ADR-0009 notes it "rewrites all Parquet per publish", i.e.
/// it collapses to one file per table. If it works on an append-shaped
/// table, self-compaction replaces the missing ducklake primitive.
#[tokio::test]
async fn s4_self_compaction_via_create_or_replace() {
    let live = live().await;
    let w = conn_for(&live.cfg, false);
    w.execute_batch("CREATE TABLE IF NOT EXISTS lake.appends (who VARCHAR, n INTEGER);")
        .unwrap();
    const N: i32 = 100;
    for i in 0..N {
        w.execute("INSERT INTO lake.appends VALUES ('w', ?)", [i])
            .unwrap();
    }
    let before = count_data_files(&w);
    let rows_before = count_rows(&w);

    let res =
        w.execute_batch("CREATE OR REPLACE TABLE lake.appends AS SELECT * FROM lake.appends;");
    let err = res.as_ref().err().map(std::string::ToString::to_string);
    let after = count_data_files(&w);
    let rows_after = count_rows(&w);

    // Old snapshots still reference the superseded files; expiry + cleanup
    // is what actually removes them from the object store.
    let _ = w.execute_batch(
        "CALL ducklake_expire_snapshots('lake', older_than => now()::TIMESTAMPTZ); \
         CALL ducklake_cleanup_old_files('lake', cleanup_all => true);",
    );
    let after_gc = count_data_files(&w);

    println!(
        "S4 RESULT: {N} appends → files before={before} after_replace={after} \
         after_gc={after_gc}, rows {rows_before}->{rows_after}, err={err:?}",
    );
    assert_eq!(
        rows_after,
        i64::from(N),
        "self-compaction must preserve rows"
    );
}
