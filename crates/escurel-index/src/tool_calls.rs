//! `run_tool_calls` — one row per `/mcp` call a run made (knowledge-
//! workbench backend P3-1). The gateway records a row for every call whose
//! bearer carries run claims: what tool, how it went, how long, how big
//! (bytes only, never payloads). The workbench reads them per run.
//!
//! Retention is the run's (owner decision 2026-09-23): no sweep here.

use duckdb::params;

use crate::{Indexer, IndexerError};

/// What the gateway records after a run-bound call.
#[derive(Debug, Clone, PartialEq)]
pub struct NewToolCall {
    pub run_id: String,
    pub root_event_id: Option<String>,
    pub tool: String,
    /// `ok` | `error`.
    pub status: String,
    pub error_code: Option<String>,
    pub duration_ms: f64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub subject: String,
}

/// One recorded call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRow {
    pub seq: i64,
    pub run_id: String,
    pub root_event_id: Option<String>,
    pub tool: String,
    pub status: String,
    pub error_code: Option<String>,
    pub duration_ms: f64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub subject: String,
    /// RFC 3339, seconds.
    pub at: String,
}

/// A page of a run's calls, oldest first.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolCallPage {
    pub calls: Vec<ToolCallRow>,
    /// The `seq` to pass back as `after` for the next page; `None` = done.
    pub next_after: Option<i64>,
}

/// The most calls one page returns.
pub const TOOL_CALLS_MAX_LIMIT: usize = 1000;

impl Indexer {
    /// Record one run-bound call. Never fails the call it describes: the
    /// gateway logs and moves on if this errs.
    ///
    /// # Errors
    /// When the insert fails.
    pub async fn record_tool_call(&self, call: NewToolCall) -> Result<i64, IndexerError> {
        let conn = self.conn.lock().await;
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_tool_calls",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO run_tool_calls \
             (seq, run_id, root_event_id, tool, status, error_code, duration_ms, \
              request_bytes, response_bytes, subject) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                seq,
                call.run_id,
                call.root_event_id,
                call.tool,
                call.status,
                call.error_code,
                call.duration_ms,
                call.request_bytes as i64,
                call.response_bytes as i64,
                call.subject,
            ],
        )?;
        Ok(seq)
    }

    /// A run's calls, oldest first, `after` the given `seq` (cursor).
    ///
    /// # Errors
    /// When the read fails.
    pub async fn list_run_tool_calls(
        &self,
        run_id: &str,
        limit: usize,
        after: Option<i64>,
    ) -> Result<ToolCallPage, IndexerError> {
        let limit = limit.clamp(1, TOOL_CALLS_MAX_LIMIT);
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT seq, run_id, root_event_id, tool, status, error_code, duration_ms, \
                    request_bytes, response_bytes, subject, strftime(at_ts, '%Y-%m-%dT%H:%M:%SZ') \
             FROM run_tool_calls WHERE run_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )?;
        let mut rows: Vec<ToolCallRow> = stmt
            .query_map(
                params![run_id, after.unwrap_or(0), (limit + 1) as i64],
                |r| {
                    Ok(ToolCallRow {
                        seq: r.get(0)?,
                        run_id: r.get(1)?,
                        root_event_id: r.get(2)?,
                        tool: r.get(3)?,
                        status: r.get(4)?,
                        error_code: r.get(5)?,
                        duration_ms: r.get(6)?,
                        request_bytes: r.get::<_, i64>(7)?.max(0) as u64,
                        response_bytes: r.get::<_, i64>(8)?.max(0) as u64,
                        subject: r.get(9)?,
                        at: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    })
                },
            )?
            .collect::<duckdb::Result<Vec<_>>>()?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next_after = if more {
            rows.last().map(|r| r.seq)
        } else {
            None
        };
        Ok(ToolCallPage {
            calls: rows,
            next_after,
        })
    }
}
