//! DuckLake attach + publish (DuckLake program, PR 3).
//!
//! [`LakeConfig`] names the lake (catalog DSN + DATA_PATH + object-store
//! credentials); the pure SQL builders ([`install_load_sql`] /
//! [`secret_sql`] / [`attach_sql`]) turn it into the statements the spike
//! validated (docs/notes/discovered/2026-07-17-ducklake-spike-results.md);
//! [`attach_lake`] executes them; [`publish_lake`] copies the indexer's
//! canonical tables into the lake as ONE DuckLake snapshot.
//!
//! Splice discipline: DuckDB has no parameter binding in ATTACH / CREATE
//! SECRET positions, so every spliced value is validated with the same
//! `is_safe_sql_fragment` guard the SQL-view backend uses (rejects quotes,
//! `;`, backslash, control chars — see `backend/sql_view.rs`). The attach
//! alias is FIXED (`lake`), never caller-supplied.

use std::sync::Arc;

use duckdb::{Connection, params};
use escurel_embed::Embedder;
use escurel_storage::LaneStore;

use super::{AdoptedIndex, PublishReport, SnapshotError};
use crate::backend::is_safe_sql_fragment;
use crate::indexer::{BLOCKS_DENSE_VEC_DIM, Indexer};
use crate::schema::Migrator;

/// The fixed attach alias. Not configurable — every publish/adopt/SQL
/// builder in this module addresses the lake as `lake.<table>`.
const LAKE_ALIAS: &str = "lake";

/// The canonical tables a publish copies into the lake, in copy order.
/// `blocks` is handled separately (its `dense_vec` needs the `FLOAT[]`
/// cast — DuckLake rejects the fixed-width `FLOAT[768]`).
/// `external_credentials` is deliberately ABSENT: secrets never leave
/// the writer (NEVER add it here).
const PUBLISH_TABLES: [&str; 5] = [
    "pages",
    "links",
    "group_members",
    "external_endpoints",
    "pack_subscriptions",
];

/// Object-store credentials for the lake's DATA_PATH, mapped 1:1 onto a
/// DuckDB `CREATE OR REPLACE SECRET` (spike-verified shapes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectStoreSecret {
    /// Local-directory DATA_PATH: no secret, no httpfs.
    None,
    /// `gs://` DATA_PATH via an HMAC key pair (`TYPE gcs`).
    Gcs { key_id: String, secret: String },
    /// `s3://` DATA_PATH (MinIO or AWS). `endpoint` is the LITERAL
    /// `host:port` (no scheme) — httpfs honours it verbatim, and a
    /// mismatch means silent unsigned PUTs (docs/spec/storage.md trap).
    S3 {
        endpoint: String,
        access_key_id: String,
        secret_access_key: String,
        region: String,
        use_ssl: bool,
    },
}

/// Where the lake lives: catalog + data path + object-store credentials.
///
/// `catalog_dsn` containing `=` is a Postgres key/value DSN
/// (`ATTACH 'ducklake:postgres:<dsn>'`); a DSN with NO `=` is treated as
/// a DuckDB-file catalog path (`ATTACH 'ducklake:<path>'`) — the
/// offline-test / dev form the spike verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LakeConfig {
    pub catalog_dsn: String,
    pub data_path: String,
    pub object_store: ObjectStoreSecret,
}

impl LakeConfig {
    /// Postgres catalog (`=` in the DSN) vs DuckDB-file catalog.
    fn is_pg_catalog(&self) -> bool {
        self.catalog_dsn.contains('=')
    }

    /// Remote DATA_PATH (`gs://` / `s3://`) needs httpfs + a secret.
    fn is_remote_data_path(&self) -> bool {
        self.data_path.starts_with("gs://") || self.data_path.starts_with("s3://")
    }
}

/// Validate every value this module splices. Fail-closed: empty values,
/// splice-unsafe characters, a secret type that disagrees with the
/// DATA_PATH scheme, or a local DATA_PATH that is not an existing
/// directory (catches a typo'd DSN-ish string landing in `data_path`).
fn validate(cfg: &LakeConfig) -> Result<(), SnapshotError> {
    let bad = |what: &str| Err(SnapshotError::InvalidLakeConfig(what.to_owned()));
    if cfg.catalog_dsn.is_empty() {
        return bad("catalog_dsn is empty");
    }
    if !is_safe_sql_fragment(&cfg.catalog_dsn) {
        return bad("catalog_dsn contains a splice-unsafe character");
    }
    if cfg.data_path.is_empty() {
        return bad("data_path is empty");
    }
    if !is_safe_sql_fragment(&cfg.data_path) {
        return bad("data_path contains a splice-unsafe character");
    }
    match (&cfg.object_store, cfg.is_remote_data_path()) {
        (ObjectStoreSecret::None, false) => {
            if !std::path::Path::new(&cfg.data_path).is_dir() {
                return bad("local data_path is not an existing directory");
            }
        }
        (ObjectStoreSecret::None, true) => {
            return bad("gs://+s3:// data_path needs an object-store secret");
        }
        (ObjectStoreSecret::Gcs { key_id, secret }, _) => {
            if !cfg.data_path.starts_with("gs://") {
                return bad("a Gcs secret requires a gs:// data_path");
            }
            if key_id.is_empty() || secret.is_empty() {
                return bad("gcs secret has an empty field");
            }
            if !is_safe_sql_fragment(key_id) || !is_safe_sql_fragment(secret) {
                return bad("gcs secret contains a splice-unsafe character");
            }
        }
        (
            ObjectStoreSecret::S3 {
                endpoint,
                access_key_id,
                secret_access_key,
                region,
                use_ssl: _,
            },
            _,
        ) => {
            if !cfg.data_path.starts_with("s3://") {
                return bad("an S3 secret requires an s3:// data_path");
            }
            if endpoint.is_empty()
                || access_key_id.is_empty()
                || secret_access_key.is_empty()
                || region.is_empty()
            {
                return bad("s3 secret has an empty field");
            }
            if ![endpoint, access_key_id, secret_access_key, region]
                .iter()
                .all(|s| is_safe_sql_fragment(s))
            {
                return bad("s3 secret contains a splice-unsafe character");
            }
        }
    }
    Ok(())
}

/// The INSTALL/LOAD prelude every lake connection runs. `INSTALL` is NOT
/// implied by a bare `LOAD` (recorded spike gotcha), so both always run;
/// `postgres` only for a Postgres catalog, `httpfs` only for a remote
/// DATA_PATH. Pure — inspectable without a DB.
#[must_use]
pub fn install_load_sql(cfg: &LakeConfig) -> String {
    let mut sql = String::from("INSTALL ducklake; LOAD ducklake;");
    if cfg.is_pg_catalog() {
        sql.push_str(" INSTALL postgres; LOAD postgres;");
    }
    if cfg.is_remote_data_path() {
        sql.push_str(" INSTALL httpfs; LOAD httpfs;");
    }
    sql
}

/// The `CREATE OR REPLACE SECRET` statement for the DATA_PATH's object
/// store, `None` for a local directory. Validates the whole config
/// (fail-closed) before splicing the credential literals.
pub fn secret_sql(cfg: &LakeConfig) -> Result<Option<String>, SnapshotError> {
    validate(cfg)?;
    Ok(match &cfg.object_store {
        ObjectStoreSecret::None => None,
        ObjectStoreSecret::Gcs { key_id, secret } => Some(format!(
            "CREATE OR REPLACE SECRET escurel_lake_store \
             (TYPE gcs, KEY_ID '{key_id}', SECRET '{secret}');"
        )),
        ObjectStoreSecret::S3 {
            endpoint,
            access_key_id,
            secret_access_key,
            region,
            use_ssl,
        } => Some(format!(
            "CREATE OR REPLACE SECRET escurel_lake_store \
             (TYPE s3, KEY_ID '{access_key_id}', SECRET '{secret_access_key}', \
              ENDPOINT '{endpoint}', URL_STYLE 'path', USE_SSL {use_ssl}, \
              REGION '{region}');"
        )),
    })
}

/// The `ATTACH IF NOT EXISTS 'ducklake:…' AS lake (DATA_PATH '…')`
/// statement (`, READ_ONLY` for readers). Validates the whole config
/// before splicing; the alias is fixed.
///
/// The writer form disables DuckLake data inlining
/// (`DATA_INLINING_ROW_LIMIT 0`): with a Postgres catalog, small writes
/// are otherwise inlined into catalog rows and NO Parquet ever reaches
/// the DATA_PATH — the publish "succeeds" while the object store stays
/// empty (see docs/notes/discovered/2026-07-17-ducklake-data-inlining.md).
pub fn attach_sql(cfg: &LakeConfig, read_only: bool) -> Result<String, SnapshotError> {
    validate(cfg)?;
    let target = if cfg.is_pg_catalog() {
        format!("ducklake:postgres:{}", cfg.catalog_dsn)
    } else {
        format!("ducklake:{}", cfg.catalog_dsn)
    };
    let opts = if read_only {
        ", READ_ONLY"
    } else {
        ", DATA_INLINING_ROW_LIMIT 0"
    };
    Ok(format!(
        "ATTACH IF NOT EXISTS '{target}' AS {LAKE_ALIAS} (DATA_PATH '{}'{opts});",
        cfg.data_path
    ))
}

/// Run the three builders on `conn`: extensions, secret (if any),
/// ATTACH. Idempotent — `ATTACH IF NOT EXISTS` makes a re-run against an
/// already-attached connection a no-op, `INSTALL`/`CREATE OR REPLACE
/// SECRET` are idempotent by construction.
pub fn attach_lake(
    conn: &Connection,
    cfg: &LakeConfig,
    read_only: bool,
) -> Result<(), SnapshotError> {
    conn.execute_batch(&install_load_sql(cfg))?;
    if let Some(sql) = secret_sql(cfg)? {
        conn.execute_batch(&sql)?;
    }
    conn.execute_batch(&attach_sql(cfg, read_only)?)?;
    Ok(())
}

/// Publish the indexer's canonical tables into the lake as ONE DuckLake
/// snapshot (one transaction = one snapshot, spike-verified — readers
/// see all-or-nothing).
///
/// Skips (cheaply, no attach) when [`Indexer::mutation_epoch`] still
/// equals `last_published_epoch`. Otherwise: take the indexer's write
/// lock (serialising against ingest), then the connection mutex for the
/// WHOLE SQL sequence (the mutex is non-reentrant — no other locking
/// `Indexer` method may run while it is held), attach idempotently, and
/// copy `pages`/`links`/`blocks`/`group_members`/`external_endpoints`/
/// `pack_subscriptions` + the single-row `escurel_manifest` in one
/// transaction. `blocks.dense_vec` is cast to `FLOAT[]` on the way out
/// (DuckLake rejects `FLOAT[768]`); `external_credentials` is NEVER
/// published.
pub async fn publish_lake(
    ix: &Indexer,
    cfg: &LakeConfig,
    last_published_epoch: Option<u64>,
) -> Result<PublishReport, SnapshotError> {
    // Lock order: write lock first (blocks new ingest from starting its
    // embed→write sequence), then the connection mutex (blocks every
    // other SQL user, including merge/rebuild paths that skip the write
    // lock). The epoch is read AFTER both are held so it names exactly
    // the state this publish snapshots.
    let _write = ix.write_guard().await;
    let mut conn = ix.conn.lock().await;
    let epoch = ix.mutation_epoch();
    if last_published_epoch == Some(epoch) {
        return Ok(PublishReport {
            snapshot_id: -1,
            epoch,
            pages: 0,
            blocks: 0,
            skipped: true,
        });
    }

    attach_lake(&conn, cfg, false)?;

    let tx = conn.transaction()?;
    for table in PUBLISH_TABLES {
        tx.execute_batch(&format!(
            "CREATE OR REPLACE TABLE {LAKE_ALIAS}.{table} AS SELECT * FROM {table};"
        ))?;
    }
    // DuckLake rejects the fixed-width FLOAT[768]; the lake carries the
    // list type and the (PR 4) adopt path casts back ::FLOAT[768].
    tx.execute_batch(&format!(
        "CREATE OR REPLACE TABLE {LAKE_ALIAS}.blocks \
         AS SELECT * REPLACE (dense_vec::FLOAT[] AS dense_vec) FROM blocks;"
    ))?;
    // Single-row manifest: CREATE OR REPLACE inside the same transaction
    // is the upsert (same snapshot as the data it describes). Pins the
    // embedding space (model_id + dim) and schema version so an adopting
    // reader can refuse a lake it cannot serve.
    tx.execute(
        &format!(
            "CREATE OR REPLACE TABLE {LAKE_ALIAS}.escurel_manifest AS \
             SELECT ?::INTEGER AS schema_version, ?::VARCHAR AS model_id, \
                    ?::INTEGER AS dim, ?::VARCHAR AS escurel_version, \
                    (SELECT count(*) FROM pages) AS pages, \
                    (SELECT count(*) FROM blocks) AS blocks, \
                    ?::BIGINT AS published_epoch"
        ),
        params![
            i64::from(Migrator::SCHEMA_VERSION),
            ix.embedder.model_id(),
            BLOCKS_DENSE_VEC_DIM as i64,
            env!("CARGO_PKG_VERSION"),
            i64::try_from(epoch).map_err(|_| {
                SnapshotError::InvalidLakeConfig("mutation epoch overflows i64".to_owned())
            })?,
        ],
    )?;
    tx.commit()?;

    // Report from the committed lake state (same connection, post-commit).
    let (pages, blocks): (i64, i64) = conn.query_row(
        &format!("SELECT pages, blocks FROM {LAKE_ALIAS}.escurel_manifest"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let snapshot_id: i64 = conn.query_row(
        &format!("SELECT max(snapshot_id) FROM ducklake_snapshots('{LAKE_ALIAS}')"),
        [],
        |r| r.get(0),
    )?;
    Ok(PublishReport {
        snapshot_id,
        epoch,
        pages: u64::try_from(pages).unwrap_or(0),
        blocks: u64::try_from(blocks).unwrap_or(0),
        skipped: false,
    })
}

/// Prune old DuckLake snapshots (+ the Parquet files only they
/// reference), keeping the `keep` most recent snapshots (DuckLake
/// program, PR 7).
///
/// `keep == 0` disables GC outright (a no-op, `Ok(0)`) — the same
/// "0 = disabled" convention `ESCUREL_SNAPSHOT_PUBLISH_SECS` uses.
/// Otherwise: read the `keep`-th-from-newest snapshot's `snapshot_time`
/// as the cutoff (`ORDER BY snapshot_id DESC LIMIT 1 OFFSET keep - 1`;
/// fewer than `keep` snapshots exist → no cutoff row → nothing to
/// prune), then `CALL ducklake_expire_snapshots(.., older_than =>
/// cutoff)` followed by `CALL ducklake_cleanup_old_files(..,
/// cleanup_all => true)` to reclaim the now-orphaned Parquet.
///
/// Verified empirically against a scratch lake (`duckdb` CLI,
/// docs/notes/discovered/2026-07-18-ducklake-snapshot-gc.md):
/// `ducklake_expire_snapshots` NEVER prunes the current (newest)
/// snapshot even when `older_than` names a time after it, and passing
/// `older_than => NULL` crashes the extension with an internal error —
/// the `Option` guard above exists specifically to avoid that call
/// shape.
///
/// Takes the indexer's write lock + connection mutex like
/// [`publish_lake`] (same non-reentrant-mutex discipline) so a
/// concurrent publish and a concurrent GC never interleave their SQL on
/// the one connection.
pub async fn gc_lake_snapshots(
    ix: &Indexer,
    cfg: &LakeConfig,
    keep: u32,
) -> Result<u64, SnapshotError> {
    if keep == 0 {
        return Ok(0);
    }
    let _write = ix.write_guard().await;
    let conn = ix.conn.lock().await;
    attach_lake(&conn, cfg, false)?;

    let cutoff: Option<String> = match conn.query_row(
        &format!(
            "SELECT CAST(snapshot_time AS VARCHAR) FROM ducklake_snapshots('{LAKE_ALIAS}') \
             ORDER BY snapshot_id DESC LIMIT 1 OFFSET ?"
        ),
        params![i64::from(keep) - 1],
        |r| r.get(0),
    ) {
        Ok(v) => Some(v),
        Err(duckdb::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(SnapshotError::LakeSql(e)),
    };
    let Some(cutoff) = cutoff else {
        // Fewer published snapshots than `keep` — nothing to prune.
        return Ok(0);
    };

    let before: i64 = conn.query_row(
        &format!("SELECT count(*) FROM ducklake_snapshots('{LAKE_ALIAS}')"),
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        &format!("CALL ducklake_expire_snapshots('{LAKE_ALIAS}', older_than => ?::TIMESTAMPTZ)"),
        params![cutoff],
    )?;
    conn.execute_batch(&format!(
        "CALL ducklake_cleanup_old_files('{LAKE_ALIAS}', cleanup_all => true);"
    ))?;
    let after: i64 = conn.query_row(
        &format!("SELECT count(*) FROM ducklake_snapshots('{LAKE_ALIAS}')"),
        [],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(before.saturating_sub(after)).unwrap_or(0))
}

/// Does `lake.<table>` exist? (information_schema over the attached
/// catalog — works on a `READ_ONLY` attach.) `table` is only ever a
/// compile-time constant here; both values bind as parameters anyway.
fn lake_table_exists(conn: &Connection, table: &str) -> Result<bool, duckdb::Error> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_catalog = ? AND table_name = ?",
        params![LAKE_ALIAS, table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Newest snapshot id in the attached lake.
fn lake_latest_snapshot(conn: &Connection) -> Result<i64, duckdb::Error> {
    conn.query_row(
        &format!("SELECT max(snapshot_id) FROM ducklake_snapshots('{LAKE_ALIAS}')"),
        [],
        |r| r.get(0),
    )
}

/// Cheap change poll: newest published lake snapshot id, `Ok(None)` when
/// the lake exists but was never published (no `escurel_manifest` yet).
///
/// Opens a throwaway in-memory scout connection, attaches `READ_ONLY`,
/// reads `max(snapshot_id) FROM ducklake_snapshots('lake')`, and drops
/// the connection (the implicit detach). Spike-measured at ≈0.2 s per
/// poll against a real Cloud SQL catalog — a 30 s cadence is ~0.7 % of
/// a core. A lake whose catalog does not exist yet (the writer has
/// never attached) surfaces as an `Err` — callers treat poll errors as
/// retryable, not fatal.
pub async fn latest_lake_snapshot_id(cfg: &LakeConfig) -> Result<Option<i64>, SnapshotError> {
    let conn = Connection::open_in_memory().map_err(|source| SnapshotError::DuckdbOpen {
        path: ":memory: (lake poll)".to_owned(),
        source,
    })?;
    attach_lake(&conn, cfg, true)?;
    if !lake_table_exists(&conn, "escurel_manifest")? {
        return Ok(None);
    }
    Ok(Some(lake_latest_snapshot(&conn)?))
}

/// Fail-closed manifest compatibility gate (loader-transfer precedent —
/// see `escurel-loader::transfer`): the lake's pinned `schema_version`
/// and embedding space (`model_id` + `dim`) must equal this reader's,
/// or the adopt refuses before loading a single row.
fn check_manifest_compat(conn: &Connection, embedder: &dyn Embedder) -> Result<(), SnapshotError> {
    let (schema_version, model_id, dim): (i64, String, i64) = conn.query_row(
        &format!("SELECT schema_version, model_id, dim FROM {LAKE_ALIAS}.escurel_manifest"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if schema_version != i64::from(Migrator::SCHEMA_VERSION) {
        return Err(SnapshotError::LakeIncompatible(format!(
            "lake schema_version {schema_version} != reader schema_version {}",
            Migrator::SCHEMA_VERSION
        )));
    }
    if model_id != embedder.model_id() {
        return Err(SnapshotError::LakeIncompatible(format!(
            "lake embedding model_id `{model_id}` != reader model_id `{}`",
            embedder.model_id()
        )));
    }
    if dim != BLOCKS_DENSE_VEC_DIM as i64 {
        return Err(SnapshotError::LakeIncompatible(format!(
            "lake embedding dim {dim} != reader dim {BLOCKS_DENSE_VEC_DIM}"
        )));
    }
    Ok(())
}

/// Adopt the newest lake snapshot into a FRESH in-memory indexer — the
/// reader half of the DuckLake snapshot cycle. `Ok(None)` when the
/// newest snapshot id equals `current` (already being served) or the
/// lake was never published.
///
/// The sequence (spike-verified, docs/notes/discovered/2026-07-17-*):
/// 1. `Connection::open_in_memory()` — MANDATORY: a file-backed DB
///    refuses `CREATE INDEX … USING HNSW` without the experimental
///    persistence flag, which readers must not rely on. `SET
///    temp_directory` points spill at a per-tenant tmp dir.
/// 2. Attach the lake `READ_ONLY` (no `DATA_INLINING_ROW_LIMIT` —
///    that's writer-side; readers must also read inlined rows).
/// 3. No-op gate (`current` == newest snapshot id), then the
///    fail-closed manifest gate ([`check_manifest_compat`]; data tables
///    without a manifest are refused too).
/// 4. Local schema: `Migrator::up` + the every-boot `ensure_*` set the
///    single-file boot runs (the DB is always fresh here, so `up`
///    always applies).
/// 5. [`Indexer::load_from_lake`]: bulk `INSERT … BY NAME` carrying
///    vectors verbatim with the `dense_vec::FLOAT[768]` cast-back,
///    HNSW built after the load, one FTS refresh; returns the snapshot
///    id read inside the SAME transaction as the copy.
/// 6. `DETACH` the lake — the adopted indexer keeps no catalog
///    connection open while serving.
///
/// The `store`/`embedder` are what [`Indexer::new`] needs at query time
/// (query embedding, blob reads); the embedder MUST be the same model
/// the writer publishes with — that is exactly what the manifest gate
/// enforces.
pub async fn adopt_lake(
    cfg: &LakeConfig,
    store: Arc<dyn LaneStore>,
    embedder: Arc<dyn Embedder>,
    tenant: &str,
    current: Option<i64>,
) -> Result<Option<AdoptedIndex>, SnapshotError> {
    let conn = Connection::open_in_memory().map_err(|source| SnapshotError::DuckdbOpen {
        path: ":memory: (lake adopt)".to_owned(),
        source,
    })?;
    set_spill_directory(&conn, tenant)?;
    // vss/fts before any DDL; the attach brings ducklake (+httpfs/postgres).
    Migrator::load_extensions(&conn)?;
    attach_lake(&conn, cfg, true)?;

    // Never-published lake: nothing to adopt — unless data tables exist
    // without a manifest, which is not "empty" but "unlabelled" → refuse.
    if !lake_table_exists(&conn, "escurel_manifest")? {
        if lake_table_exists(&conn, "pages")? {
            return Err(SnapshotError::LakeIncompatible(
                "lake has data tables but no escurel_manifest".to_owned(),
            ));
        }
        return Ok(None);
    }
    // No-op gate before the (heavier) local schema work.
    let latest = lake_latest_snapshot(&conn)?;
    if current == Some(latest) {
        return Ok(None);
    }
    check_manifest_compat(&conn, embedder.as_ref())?;

    // Local schema — mirror the single-file boot's every-boot set. The
    // DB is always fresh here so `up` always runs; NO
    // `enable_hnsw_persistence` beyond the one `up` itself sets
    // (harmless no-op in-memory), the reader never persists HNSW.
    Migrator::up(&conn)?;
    Migrator::ensure_group_members(&conn)?;
    Migrator::ensure_external_credentials(&conn)?;
    Migrator::ensure_external_endpoints(&conn)?;
    Migrator::ensure_pack_subscriptions(&conn)?;
    Migrator::ensure_block_context(&conn)?;

    // `contextualize` is not mirrored: it is an INGEST-side knob and the
    // adopted indexer never ingests; the default mode is correct here.
    let indexer = Indexer::new(store, embedder, conn, tenant)?;
    let snapshot_id = indexer.load_from_lake(LAKE_ALIAS).await?;
    {
        let conn = indexer.conn.lock().await;
        conn.execute_batch(&format!("DETACH {LAKE_ALIAS};"))?;
    }
    Ok(Some(AdoptedIndex {
        indexer: Arc::new(indexer),
        snapshot_id,
    }))
}

/// `SET temp_directory` so the in-memory reader can spill large
/// intermediates to disk instead of OOMing (nothing else in the
/// codebase sets one — file-backed DBs default to `<db>.tmp`, but an
/// in-memory DB has no default spill location). Per-tenant path under
/// the OS temp dir; fail-closed on a splice-unsafe path like every
/// other spliced literal in this module.
fn set_spill_directory(conn: &Connection, tenant: &str) -> Result<(), SnapshotError> {
    let dir = std::env::temp_dir().join(format!("escurel-adopt-{tenant}"));
    std::fs::create_dir_all(&dir).map_err(|source| SnapshotError::DataDir {
        path: dir.display().to_string(),
        source,
    })?;
    let dir = dir.display().to_string();
    if !is_safe_sql_fragment(&dir) {
        return Err(SnapshotError::InvalidLakeConfig(
            "temp_directory path contains a splice-unsafe character".to_owned(),
        ));
    }
    conn.execute_batch(&format!("SET temp_directory = '{dir}';"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_cfg(dir: &std::path::Path) -> LakeConfig {
        LakeConfig {
            catalog_dsn: "/tmp/cat.ducklake".to_owned(),
            data_path: dir.to_str().unwrap().to_owned(),
            object_store: ObjectStoreSecret::None,
        }
    }

    fn pg_s3_cfg() -> LakeConfig {
        LakeConfig {
            catalog_dsn: "host=127.0.0.1 port=5432 user=u password=p dbname=d".to_owned(),
            data_path: "s3://lake/data/".to_owned(),
            object_store: ObjectStoreSecret::S3 {
                endpoint: "127.0.0.1:9000".to_owned(),
                access_key_id: "minioadmin".to_owned(),
                secret_access_key: "minioadmin".to_owned(),
                region: "us-east-1".to_owned(),
                use_ssl: false,
            },
        }
    }

    #[test]
    fn install_load_covers_every_branch() {
        let tmp = tempfile::tempdir().unwrap();
        // Local file catalog + local dir: ducklake only.
        let sql = install_load_sql(&local_cfg(tmp.path()));
        assert!(sql.contains("INSTALL ducklake; LOAD ducklake;"));
        assert!(!sql.contains("postgres"));
        assert!(!sql.contains("httpfs"));
        // PG catalog + s3 data: all three.
        let sql = install_load_sql(&pg_s3_cfg());
        assert!(sql.contains("INSTALL postgres; LOAD postgres;"));
        assert!(sql.contains("INSTALL httpfs; LOAD httpfs;"));
        // gs:// data path: httpfs, and gcs secret shape below.
        let gcs = LakeConfig {
            catalog_dsn: "host=h user=u".to_owned(),
            data_path: "gs://bucket/prefix/".to_owned(),
            object_store: ObjectStoreSecret::Gcs {
                key_id: "K".to_owned(),
                secret: "S".to_owned(),
            },
        };
        assert!(install_load_sql(&gcs).contains("httpfs"));
    }

    #[test]
    fn attach_sql_pg_vs_file_catalog_and_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let local = attach_sql(&local_cfg(tmp.path()), false).unwrap();
        assert!(
            local.starts_with("ATTACH IF NOT EXISTS 'ducklake:/tmp/cat.ducklake' AS lake"),
            "{local}"
        );
        assert!(!local.contains("READ_ONLY"));
        // Writers disable data inlining so Parquet always reaches the
        // DATA_PATH; readers must NOT carry the (write-side) option.
        assert!(local.contains("DATA_INLINING_ROW_LIMIT 0"), "{local}");

        let pg = attach_sql(&pg_s3_cfg(), true).unwrap();
        assert!(
            pg.contains("'ducklake:postgres:host=127.0.0.1 port=5432"),
            "{pg}"
        );
        assert!(
            pg.contains("DATA_PATH 's3://lake/data/', READ_ONLY"),
            "{pg}"
        );
    }

    #[test]
    fn secret_sql_per_scheme() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(secret_sql(&local_cfg(tmp.path())).unwrap(), None);

        let s3 = secret_sql(&pg_s3_cfg()).unwrap().unwrap();
        assert!(s3.contains("TYPE s3"), "{s3}");
        assert!(s3.contains("ENDPOINT '127.0.0.1:9000'"), "{s3}");
        assert!(s3.contains("URL_STYLE 'path'"), "{s3}");
        assert!(s3.contains("USE_SSL false"), "{s3}");
        assert!(s3.contains("REGION 'us-east-1'"), "{s3}");

        let gcs = LakeConfig {
            catalog_dsn: "host=h user=u".to_owned(),
            data_path: "gs://b/p/".to_owned(),
            object_store: ObjectStoreSecret::Gcs {
                key_id: "K".to_owned(),
                secret: "S".to_owned(),
            },
        };
        let sql = secret_sql(&gcs).unwrap().unwrap();
        assert!(sql.contains("TYPE gcs, KEY_ID 'K', SECRET 'S'"), "{sql}");
    }

    #[test]
    fn rejects_unsafe_and_inconsistent_configs() {
        let tmp = tempfile::tempdir().unwrap();
        let assert_rejected = |cfg: &LakeConfig, why: &str| {
            assert!(
                matches!(
                    attach_sql(cfg, false),
                    Err(SnapshotError::InvalidLakeConfig(_))
                ),
                "must reject: {why}"
            );
        };

        let mut cfg = local_cfg(tmp.path());
        cfg.catalog_dsn = String::new();
        assert_rejected(&cfg, "empty catalog_dsn");

        let mut cfg = local_cfg(tmp.path());
        cfg.catalog_dsn = "x'; DROP TABLE pages; --".to_owned();
        assert_rejected(&cfg, "splice-unsafe catalog_dsn");

        let mut cfg = local_cfg(tmp.path());
        cfg.data_path = String::new();
        assert_rejected(&cfg, "empty data_path");

        let mut cfg = local_cfg(tmp.path());
        cfg.data_path = "/x'; DROP TABLE pages; --".to_owned();
        assert_rejected(&cfg, "splice-unsafe data_path");

        let mut cfg = local_cfg(tmp.path());
        cfg.data_path = tmp.path().join("no-such-dir").to_str().unwrap().to_owned();
        assert_rejected(&cfg, "local data_path that is not a directory");

        let mut cfg = local_cfg(tmp.path());
        cfg.data_path = "s3://bucket/x/".to_owned();
        assert_rejected(&cfg, "remote data_path without a secret");

        let mut cfg = pg_s3_cfg();
        cfg.data_path = tmp.path().to_str().unwrap().to_owned();
        assert_rejected(&cfg, "S3 secret with a local data_path");

        let mut cfg = pg_s3_cfg();
        cfg.object_store = ObjectStoreSecret::Gcs {
            key_id: "K".to_owned(),
            secret: "S".to_owned(),
        };
        assert_rejected(&cfg, "Gcs secret with an s3:// data_path");

        let mut cfg = pg_s3_cfg();
        if let ObjectStoreSecret::S3 {
            secret_access_key, ..
        } = &mut cfg.object_store
        {
            *secret_access_key = "p'; DROP SECRET x; --".to_owned();
        }
        assert!(
            matches!(secret_sql(&cfg), Err(SnapshotError::InvalidLakeConfig(_))),
            "must reject a splice-unsafe s3 secret"
        );

        let gcs_empty = LakeConfig {
            catalog_dsn: "host=h".to_owned(),
            data_path: "gs://b/".to_owned(),
            object_store: ObjectStoreSecret::Gcs {
                key_id: String::new(),
                secret: "S".to_owned(),
            },
        };
        assert!(
            matches!(
                secret_sql(&gcs_empty),
                Err(SnapshotError::InvalidLakeConfig(_))
            ),
            "must reject an empty gcs key_id"
        );
    }

    #[test]
    fn publish_never_names_external_credentials() {
        // Belt-and-braces: the copy list must never grow the credential
        // registry (secrets never leave the writer).
        assert!(!PUBLISH_TABLES.contains(&"external_credentials"));
    }
}
