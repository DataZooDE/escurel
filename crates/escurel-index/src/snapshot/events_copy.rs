//! `events-copy`: move the append surfaces from the attached Postgres tables to the lake.
//!
//! `ESCUREL_EVENTS_BACKEND` / `ESCUREL_CHAT_BACKEND` pick where `escurel_events`, `escurel_drafts` and
//! `escurel_chat_messages` live: the catalog database (Postgres, the default) or the lake (`ducklake`).
//! Switching the variable alone starts EMPTY lake tables and leaves the Postgres rows unreachable: the inbox
//! items, the pending drafts (they follow the events selector), the audit events and the chat. This module
//! is the supported copy that makes the switch lossless.
//!
//! Shape, deliberately boring:
//!
//! - **Stopped writer.** The caller holds the single-writer lease (the CLI does, like `lake-upgrade`).
//! - **Read-only source.** The Postgres tables are attached `READ_ONLY`; no source row is ever modified or
//!   deleted, so rolling back is switching the environment back.
//! - **Per tenant.** The source tables are shared by every tenant of the database; only `tenant = X` moves.
//! - **Idempotent.** A row is copied only when no destination row has its key (`event_id`, `draft_id`,
//!   `msg_id`), in ONE statement per table: one atomic lake snapshot and one Parquet file, so a failed run
//!   leaves a table untouched or complete, and a re-run copies exactly what is missing. A destination row
//!   that exists but differs from its source is reported (`diverged`) and refuses the apply; the copy never
//!   overwrites.
//! - **Verified.** After the insert each table's row count and an order-independent checksum over every copied
//!   column must match between the source and the destination rows with the source's keys.
//! - **Audited.** An applied run that copied rows writes one `escurel:events-copy` system event (counts in the
//!   body) into the lake events table, with the next `seq`.
//! - **Dry run is the default** and writes nothing, not even the destination tables.

use std::path::Path;

use duckdb::{Connection, params};
use serde::Serialize;

use super::append_lake::{
    APPEND_LAKE_ALIAS, attach_chat_lake, attach_drafts_lake, attach_events_lake,
};
use super::lake::LAKE_ALIAS;
use super::{LakeConfig, SnapshotError, attach_sql, install_load_sql, secret_sql};
use crate::backend::is_safe_sql_fragment;
use crate::chat::CHAT_PG_TABLE_NAME;
use crate::drafts::DRAFTS_PG_TABLE_NAME;
use crate::events::{EVENTS_PG_TABLE_NAME, now_rfc3339_micros};

/// Fixed ATTACH alias for the read-only Postgres source.
const SOURCE_ALIAS: &str = "events_copy_src";

/// The label of the audit event an applied copy leaves behind.
pub const EVENTS_COPY_LABEL: &str = "escurel:events-copy";

/// Which append surface to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTable {
    Events,
    Drafts,
    Chat,
}

impl CopyTable {
    /// Copy order: drafts reference events, so events go first.
    pub const ALL: &'static [CopyTable] = &[CopyTable::Events, CopyTable::Drafts, CopyTable::Chat];

    /// Parse the CLI spelling (`events`, `drafts`, `chat`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "events" => Some(Self::Events),
            "drafts" => Some(Self::Drafts),
            "chat" => Some(Self::Chat),
            _ => None,
        }
    }

    fn spec(self) -> Spec {
        match self {
            Self::Events => Spec {
                table: EVENTS_PG_TABLE_NAME,
                key: &["event_id"],
                // `created_at` is Postgres-only (the lake events table has none); `seq` is copied as stored so the
                // tail cursors and the ordering survive.
                columns: &[
                    "tenant",
                    "event_id",
                    "at_ts",
                    "source",
                    "mime",
                    "label_skill",
                    "instance_page_id",
                    "status",
                    "title",
                    "body",
                    "provenance",
                    "kind",
                    "root_event_id",
                    "run_id",
                    "seq",
                    "revision_binding_sha256",
                ],
                order: "s.seq NULLS LAST, s.at_ts NULLS LAST, s.event_id",
            },
            Self::Drafts => Spec {
                table: DRAFTS_PG_TABLE_NAME,
                key: &["draft_id"],
                columns: &[
                    "tenant",
                    "draft_id",
                    "target_page_id",
                    "content",
                    "content_sha256",
                    "base_sha256",
                    "author",
                    "event_id",
                    "status",
                    "reason",
                    "decided_by",
                    "changeset_id",
                    "created_at",
                    "decided_at",
                    "base_version",
                    "run_id",
                    "root_event_id",
                    "seq",
                ],
                order: "s.created_at NULLS LAST, s.draft_id",
            },
            Self::Chat => Spec {
                table: CHAT_PG_TABLE_NAME,
                key: &["msg_id"],
                columns: &[
                    "tenant",
                    "chat_group_id",
                    "msg_id",
                    "ts",
                    "role",
                    "author",
                    "content",
                    "metadata",
                    "dense_vec",
                    "embedded",
                    "created_at",
                ],
                order: "s.ts, s.msg_id",
            },
        }
    }
}

struct Spec {
    table: &'static str,
    key: &'static [&'static str],
    columns: &'static [&'static str],
    order: &'static str,
}

/// What to copy, from where.
#[derive(Debug, Clone)]
pub struct EventsCopyOptions<'a> {
    /// The Postgres database holding the shared tables: the lake catalog's DSN.
    pub source_dsn: &'a str,
    pub tenant: &'a str,
    /// `false` (the default) is a dry run: counts only, nothing is created or written.
    pub apply: bool,
    pub tables: &'a [CopyTable],
}

/// One table's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableCopyReport {
    pub table: String,
    /// Rows of this tenant in the Postgres source.
    pub source_rows: i64,
    /// Source rows whose key already exists in the lake.
    pub already_present: i64,
    /// Of those, rows whose content differs from the source (a non-zero value refuses the apply).
    pub diverged: i64,
    /// Rows an apply would copy now.
    pub to_copy: i64,
    /// Rows this run copied (`0` for a dry run).
    pub copied: i64,
    /// Order-independent checksum over every copied column of the source rows.
    pub source_checksum: Option<String>,
    /// The same checksum over the destination rows with the source's keys: equal to `source_checksum` once
    /// everything is copied.
    pub dest_checksum: Option<String>,
    /// Why nothing was done for this table, e.g. the source table does not exist.
    pub note: Option<String>,
}

/// The whole run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventsCopyReport {
    pub applied: bool,
    pub tenant: String,
    pub tables: Vec<TableCopyReport>,
    /// The audit event an applied run that copied rows wrote.
    pub audit_event_id: Option<String>,
}

fn copy_err(msg: impl Into<String>) -> SnapshotError {
    SnapshotError::EventsCopy(msg.into())
}

/// Copy `opts.tables` of `opts.tenant` from the Postgres source into the lake.
///
/// The caller must hold the writer lease (or know the writer is stopped); nothing here checks.
///
/// # Errors
/// An unreachable source or lake, a destination row that differs from its source, or a verification mismatch
/// after the insert.
pub async fn events_copy(
    lake: &LakeConfig,
    opts: &EventsCopyOptions<'_>,
) -> Result<EventsCopyReport, SnapshotError> {
    if opts.tenant.is_empty() || !is_safe_sql_fragment(opts.tenant) {
        return Err(copy_err(
            "tenant is empty or contains a splice-unsafe character",
        ));
    }
    if opts.source_dsn.is_empty() || !is_safe_sql_fragment(opts.source_dsn) {
        return Err(copy_err(
            "source DSN is empty or contains a splice-unsafe character",
        ));
    }
    if opts.tables.is_empty() {
        return Err(copy_err("no tables selected (events, drafts, chat)"));
    }
    let conn = Connection::open_in_memory().map_err(|source| SnapshotError::DuckdbOpen {
        path: ":memory: (events-copy)".to_owned(),
        source,
    })?;
    conn.execute_batch("INSTALL postgres; LOAD postgres;")?;
    conn.execute_batch(&format!(
        "ATTACH '{}' AS {SOURCE_ALIAS} (TYPE postgres, READ_ONLY);",
        opts.source_dsn
    ))?;

    let dest_attached = if opts.apply {
        // The audit event always needs the events table; the others only when selected.
        attach_events_lake(&conn, lake)?;
        if opts.tables.contains(&CopyTable::Drafts) {
            attach_drafts_lake(&conn, lake)?;
        }
        if opts.tables.contains(&CopyTable::Chat) {
            attach_chat_lake(&conn, lake)?;
        }
        true
    } else {
        attach_dest_read_only(&conn, lake)?
    };

    // Pass 1: look, for every table, before writing anything.
    let mut analysed = Vec::new();
    for &t in opts.tables {
        analysed.push((t, analyse(&conn, t, opts.tenant, dest_attached)?));
    }
    if opts.apply {
        let diverged: Vec<String> = analysed
            .iter()
            .filter(|(_, r)| r.diverged > 0)
            .map(|(_, r)| format!("{} ({} rows)", r.table, r.diverged))
            .collect();
        if !diverged.is_empty() {
            return Err(copy_err(format!(
                "refused: destination rows differ from their source in {}; \
                 the copy never overwrites (inspect the lake rows, then fix or delete them)",
                diverged.join(", ")
            )));
        }
    }

    // Pass 2: copy, one statement per table, then verify.
    let mut tables = Vec::new();
    for (t, mut report) in analysed {
        if opts.apply && report.to_copy > 0 {
            let spec = t.spec();
            let (src_cols, _) = column_sets(&conn, &spec)?;
            conn.execute(&insert_sql(&spec, &src_cols), params![opts.tenant])?;
            report.copied = report.to_copy;
            verify(&conn, &spec, &src_cols, opts.tenant, &mut report)?;
        }
        tables.push(report);
    }

    let copied_total: i64 = tables.iter().map(|t| t.copied).sum();
    let audit_event_id = if opts.apply && copied_total > 0 {
        Some(write_audit(&conn, opts.tenant, &tables)?)
    } else {
        None
    };
    Ok(EventsCopyReport {
        applied: opts.apply,
        tenant: opts.tenant.to_owned(),
        tables,
        audit_event_id,
    })
}

/// Attach the lake under the append alias WITHOUT creating anything. `Ok(false)` when there is no lake yet
/// (a fresh file catalog): the destination is then simply empty.
fn attach_dest_read_only(conn: &Connection, lake: &LakeConfig) -> Result<bool, SnapshotError> {
    if lake.is_pg_catalog() {
        // A Postgres catalog that has never held a lake has no `ducklake_snapshot`; a READ_ONLY attach would
        // refuse it ("creating a new DuckLake is explicitly disabled").
        if !table_exists(conn, SOURCE_ALIAS, "ducklake_snapshot")? {
            return Ok(false);
        }
    } else if !Path::new(&lake.catalog_dsn).exists() {
        return Ok(false);
    }
    conn.execute_batch(&install_load_sql(lake))?;
    if let Some(sql) = secret_sql(lake)? {
        conn.execute_batch(&sql)?;
    }
    // The alias is a fixed literal on both sides, never caller-supplied.
    let sql = attach_sql(lake, true)?.replace(
        &format!(" AS {LAKE_ALIAS} "),
        &format!(" AS {APPEND_LAKE_ALIAS} "),
    );
    conn.execute_batch(&sql)?;
    Ok(true)
}

fn table_exists(conn: &Connection, db: &str, table: &str) -> Result<bool, SnapshotError> {
    Ok(conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_tables() WHERE database_name = ? AND table_name = ?",
        params![db, table],
        |r| r.get(0),
    )?)
}

fn columns_of(conn: &Connection, db: &str, table: &str) -> Result<Vec<String>, SnapshotError> {
    let mut stmt = conn.prepare(
        "SELECT column_name FROM duckdb_columns() WHERE database_name = ? AND table_name = ?",
    )?;
    let rows = stmt.query_map(params![db, table], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `(columns present in the source, columns present in the destination)`.
fn column_sets(
    conn: &Connection,
    spec: &Spec,
) -> Result<(Vec<String>, Vec<String>), SnapshotError> {
    Ok((
        columns_of(conn, SOURCE_ALIAS, spec.table)?,
        columns_of(conn, APPEND_LAKE_ALIAS, spec.table)?,
    ))
}

/// The select list reading each wanted column from `alias`, or `NULL` where the table does not have it (a
/// source provisioned before a column existed).
fn exprs(spec: &Spec, alias: &str, present: &[String]) -> Vec<String> {
    spec.columns
        .iter()
        .map(|c| {
            if present.iter().any(|p| p == c) {
                format!("{alias}.\"{c}\"")
            } else {
                "NULL".to_owned()
            }
        })
        .collect()
}

fn key_join(spec: &Spec) -> String {
    let mut parts = vec!["d.tenant = s.tenant".to_owned()];
    parts.extend(spec.key.iter().map(|k| format!("d.\"{k}\" = s.\"{k}\"")));
    parts.join(" AND ")
}

fn insert_sql(spec: &Spec, src_cols: &[String]) -> String {
    let cols = spec
        .columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {APPEND_LAKE_ALIAS}.{t} ({cols}) \
         SELECT {sel} FROM {SOURCE_ALIAS}.{t} s \
         WHERE s.tenant = ? AND NOT EXISTS \
           (SELECT 1 FROM {APPEND_LAKE_ALIAS}.{t} d WHERE {join}) \
         ORDER BY {order}",
        t = spec.table,
        sel = exprs(spec, "s", src_cols).join(", "),
        join = key_join(spec),
        order = spec.order,
    )
}

/// Count and compare one table. Reads only.
fn analyse(
    conn: &Connection,
    t: CopyTable,
    tenant: &str,
    dest_attached: bool,
) -> Result<TableCopyReport, SnapshotError> {
    let spec = t.spec();
    let mut report = TableCopyReport {
        table: spec.table.to_owned(),
        source_rows: 0,
        already_present: 0,
        diverged: 0,
        to_copy: 0,
        copied: 0,
        source_checksum: None,
        dest_checksum: None,
        note: None,
    };
    if !table_exists(conn, SOURCE_ALIAS, spec.table)? {
        report.note = Some("the source table does not exist: nothing to copy".to_owned());
        return Ok(report);
    }
    let src_cols = columns_of(conn, SOURCE_ALIAS, spec.table)?;
    report.source_rows = conn.query_row(
        &format!(
            "SELECT count(*) FROM {SOURCE_ALIAS}.{} s WHERE s.tenant = ?",
            spec.table
        ),
        params![tenant],
        |r| r.get(0),
    )?;
    let src_hash = format!("hash({})", exprs(&spec, "s", &src_cols).join(", "));
    report.source_checksum = conn.query_row(
        &format!(
            "SELECT CAST(sum({src_hash}) AS VARCHAR) FROM {SOURCE_ALIAS}.{} s WHERE s.tenant = ?",
            spec.table
        ),
        params![tenant],
        |r| r.get(0),
    )?;
    let dest_exists = dest_attached && table_exists(conn, APPEND_LAKE_ALIAS, spec.table)?;
    if dest_exists {
        let dst_cols = columns_of(conn, APPEND_LAKE_ALIAS, spec.table)?;
        let join = key_join(&spec);
        report.already_present = conn.query_row(
            &format!(
                "SELECT count(*) FROM {SOURCE_ALIAS}.{t} s WHERE s.tenant = ? AND EXISTS \
                   (SELECT 1 FROM {APPEND_LAKE_ALIAS}.{t} d WHERE {join})",
                t = spec.table
            ),
            params![tenant],
            |r| r.get(0),
        )?;
        // A column the destination does not have yet (an old lake table, dry run) reads as NULL on both sides.
        let both: Vec<String> = src_cols
            .iter()
            .filter(|c| dst_cols.contains(c))
            .cloned()
            .collect();
        let hs = format!("hash({})", exprs(&spec, "s", &both).join(", "));
        let hd = format!("hash({})", exprs(&spec, "d", &both).join(", "));
        report.diverged = conn.query_row(
            &format!(
                "SELECT count(*) FROM {SOURCE_ALIAS}.{t} s JOIN {APPEND_LAKE_ALIAS}.{t} d ON {join} \
                 WHERE s.tenant = ? AND {hs} <> {hd}",
                t = spec.table
            ),
            params![tenant],
            |r| r.get(0),
        )?;
        let hd_all = format!("hash({})", exprs(&spec, "d", &dst_cols).join(", "));
        report.dest_checksum = conn.query_row(
            &format!(
                "SELECT CAST(sum({hd_all}) AS VARCHAR) FROM {SOURCE_ALIAS}.{t} s \
                 JOIN {APPEND_LAKE_ALIAS}.{t} d ON {join} WHERE s.tenant = ?",
                t = spec.table
            ),
            params![tenant],
            |r| r.get(0),
        )?;
    }
    report.to_copy = report.source_rows - report.already_present;
    Ok(report)
}

/// After the insert: every source row must now have its destination twin, and the checksums must agree.
fn verify(
    conn: &Connection,
    spec: &Spec,
    src_cols: &[String],
    tenant: &str,
    report: &mut TableCopyReport,
) -> Result<(), SnapshotError> {
    let dst_cols = columns_of(conn, APPEND_LAKE_ALIAS, spec.table)?;
    let join = key_join(spec);
    let matched: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM {SOURCE_ALIAS}.{t} s JOIN {APPEND_LAKE_ALIAS}.{t} d ON {join} \
             WHERE s.tenant = ?",
            t = spec.table
        ),
        params![tenant],
        |r| r.get(0),
    )?;
    let hs = format!("hash({})", exprs(spec, "s", src_cols).join(", "));
    let hd = format!("hash({})", exprs(spec, "d", &dst_cols).join(", "));
    let (src_sum, dst_sum): (Option<String>, Option<String>) = conn.query_row(
        &format!(
            "SELECT CAST(sum({hs}) AS VARCHAR), CAST(sum({hd}) AS VARCHAR) \
             FROM {SOURCE_ALIAS}.{t} s JOIN {APPEND_LAKE_ALIAS}.{t} d ON {join} WHERE s.tenant = ?",
            t = spec.table
        ),
        params![tenant],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if matched != report.source_rows || src_sum != dst_sum {
        return Err(copy_err(format!(
            "verification failed for {}: {} source rows, {matched} found in the lake, checksums {src_sum:?} vs {dst_sum:?}",
            spec.table, report.source_rows
        )));
    }
    report.source_checksum = src_sum;
    report.dest_checksum = dst_sum;
    Ok(())
}

/// One `escurel:events-copy` system event with the counts, at the next `seq`.
fn write_audit(
    conn: &Connection,
    tenant: &str,
    tables: &[TableCopyReport],
) -> Result<String, SnapshotError> {
    let at = now_rfc3339_micros();
    let event_id = format!("events-copy:{at}");
    let body = serde_json::json!({
        "tables": tables.iter().map(|t| serde_json::json!({
            "table": t.table,
            "source_rows": t.source_rows,
            "copied": t.copied,
            "already_present": t.already_present,
            "checksum": t.dest_checksum,
        })).collect::<Vec<_>>(),
    })
    .to_string();
    conn.execute(
        &format!(
            "INSERT INTO {APPEND_LAKE_ALIAS}.{EVENTS_PG_TABLE_NAME} \
               (tenant, event_id, at_ts, source, mime, label_skill, instance_page_id, status, title, body, \
                provenance, kind, root_event_id, run_id, seq, revision_binding_sha256) \
             SELECT ?, ?, TRY_CAST(? AS TIMESTAMP), 'escurel-server', 'application/json', \
                    '{EVENTS_COPY_LABEL}', NULL, 'inbox', 'events-copy', ?, NULL, 'system', ?, NULL, \
                    COALESCE((SELECT MAX(seq) FROM {APPEND_LAKE_ALIAS}.{EVENTS_PG_TABLE_NAME} \
                              WHERE tenant = ?), 0) + 1, NULL"
        ),
        params![tenant, event_id, at, body, event_id, tenant],
    )?;
    Ok(event_id)
}
