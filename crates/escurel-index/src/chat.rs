//! Per-chat-group conversation history surface
//! (DataZooDE/escurel#63 — M-Chat).
//!
//! `chat_messages` is an append-mostly log, distinct from the typed
//! `pages` / `blocks` knowledge base. It sidesteps three frictions
//! the issue calls out:
//!
//! - `update_page` is whole-page; appending one message rewrites the
//!   whole page, which is O(history) per append.
//! - Every page block is embedded; chat messages don't justify the
//!   cost for high-volume sources, so embedding is opt-out per
//!   [`AppendChatMessage::embed`].
//! - The 12-tool surface had no append/read-back primitive for raw
//!   conversation; this module is the missing piece, sitting next to
//!   the KB rather than inside it.
//!
//! Rows live in the same per-tenant DuckDB file as the rest of the
//! index, with the per-tenant `Mutex<Connection>` from
//! [`crate::Indexer`] serialising writes. Non-embedded rows hold
//! `dense_vec = NULL`; HNSW similarity queries filter
//! `WHERE dense_vec IS NOT NULL`
//! (docs/notes/discovered/2026-05-25-vss-hnsw-tolerates-null-rows.md).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use escurel_embed::EmbedError;
use ulid::Ulid;

use crate::indexer::{BLOCKS_DENSE_VEC_DIM, Indexer, IndexerError, format_vector_literal};
use crate::read::OrderDir;

/// Table name for the shared attached-Postgres chat table (DuckLake PR 8,
/// Phase B). Lives here (not `snapshot::chat_pg`) because `chat.rs` owns
/// the chat concept; `snapshot::chat_pg` imports it back for the
/// `CREATE TABLE` DDL so the name is defined exactly once.
pub const CHAT_PG_TABLE_NAME: &str = "escurel_chat_messages";

/// Which physical table [`Indexer`]'s chat methods (`append_chat_message`
/// / `list_chat_messages` / `delete_chat_history` /
/// `search_chat_messages`) read and write.
///
/// `Local` (the default `Indexer::new` construction) is today's
/// single-file behaviour, byte-identical: the per-tenant `chat_messages`
/// table, no `tenant` column (tenancy is implicit — one DuckDB file per
/// tenant). `AttachedPostgres` (DuckLake PR 8) points every ducklake
/// replica — writer AND every reader — at ONE shared, writable Postgres
/// table (`snapshot::attach_chat_pg`), scoped by an explicit `tenant`
/// column since the physical table is no longer implicitly
/// single-tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatBackend {
    /// The local per-tenant `chat_messages` table.
    Local,
    /// An attached, read-write Postgres table shared by every replica.
    /// `alias` is the DuckDB `ATTACH` alias (`snapshot::CHAT_PG_ALIAS`,
    /// duplicated here as a plain `String` rather than a crate
    /// cross-reference so `chat.rs` has no dependency on the `snapshot`
    /// module — the alias is a SQL identifier, not shared state).
    AttachedPostgres { alias: String },
}

/// Input to [`Indexer::search_chat_messages`].
#[derive(Debug, Clone)]
pub struct SearchChatMessages<'a> {
    pub chat_group_id: &'a str,
    pub query: &'a str,
    pub limit: usize,
}

/// One message in a chat-group's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub chat_group_id: String,
    pub msg_id: String,
    /// RFC 3339 in UTC; second precision matches the test corpus and
    /// avoids depending on the host's locale.
    pub ts: String,
    pub role: String,
    pub author: Option<String>,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub embedded: bool,
}

/// Input to [`Indexer::append_chat_message`].
///
/// `chat_group_id` is opaque to escurel — consumers (e.g. Carl)
/// choose the identifier scheme (room IDs, DM pair IDs, …). `ts` is
/// the event time; when `None`, the server stamps it with
/// `CURRENT_TIMESTAMP` inside the same statement. `msg_id` is
/// caller-supplied (e.g. when re-ingesting from an external source)
/// or server-generated as a ULID.
#[derive(Debug, Clone)]
pub struct AppendChatMessage<'a> {
    pub chat_group_id: &'a str,
    pub role: &'a str,
    pub content: &'a str,
    pub author: Option<&'a str>,
    pub ts: Option<&'a str>,
    pub metadata: Option<serde_json::Value>,
    pub msg_id: Option<&'a str>,
    pub embed: bool,
}

/// Input to [`Indexer::list_chat_messages`].
///
/// `since` is inclusive, `until` is exclusive — a half-open interval
/// that composes cleanly with retention windows (passing `until = T`
/// to `list_chat_messages` and `before_ts = T` to
/// `delete_chat_history` removes exactly the prior page). `limit` is
/// capped at [`ChatPage::MAX_LIMIT`]; `cursor` is the `next_cursor`
/// returned by a previous call.
#[derive(Debug, Clone)]
pub struct ListChatMessages<'a> {
    pub chat_group_id: &'a str,
    pub since: Option<&'a str>,
    pub until: Option<&'a str>,
    pub limit: usize,
    pub cursor: Option<&'a str>,
    pub direction: OrderDir,
}

/// One page of [`ChatMessage`]s returned by
/// [`Indexer::list_chat_messages`]. `next_cursor` is present only
/// when there is at least one row past the page.
#[derive(Debug, Clone)]
pub struct ChatPage {
    pub messages: Vec<ChatMessage>,
    pub next_cursor: Option<String>,
}

impl ChatPage {
    /// Hard cap on `limit`. The list path is read-only and per-page
    /// payloads need a ceiling so a buggy consumer cannot drain the
    /// connection on one call.
    pub const MAX_LIMIT: usize = 1000;
}

/// Cursor payload. Encoded as base64url(`"<ts>|<msg_id>"`). Decoded
/// by the next `list_chat_messages` call to resume after
/// `(ts, msg_id)` in the requested direction. The `|` separator is
/// safe — RFC-3339 timestamps and Crockford-base32 ULIDs do not
/// contain it.
#[derive(Debug)]
struct Cursor {
    ts: String,
    msg_id: String,
}

impl Cursor {
    fn encode(&self) -> String {
        let raw = format!("{}|{}", self.ts, self.msg_id);
        URL_SAFE_NO_PAD.encode(raw.as_bytes())
    }

    fn decode(raw: &str) -> Result<Self, IndexerError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(raw.as_bytes())
            .map_err(|e| IndexerError::InvalidCursor(format!("base64: {e}")))?;
        let s = std::str::from_utf8(&bytes)
            .map_err(|e| IndexerError::InvalidCursor(format!("utf-8: {e}")))?;
        let (ts, msg_id) = s
            .split_once('|')
            .ok_or_else(|| IndexerError::InvalidCursor("missing separator".to_owned()))?;
        Ok(Cursor {
            ts: ts.to_owned(),
            msg_id: msg_id.to_owned(),
        })
    }
}

impl Indexer {
    /// The table this indexer's chat methods read/write:
    /// `chat_messages` (local) or `<alias>.escurel_chat_messages`
    /// (attached Postgres, DuckLake PR 8).
    fn chat_table(&self) -> String {
        match self.chat_backend() {
            ChatBackend::Local => "chat_messages".to_owned(),
            ChatBackend::AttachedPostgres { alias } => format!("{alias}.{CHAT_PG_TABLE_NAME}"),
        }
    }

    /// `Some(tenant)` when chat rows must be scoped by an explicit
    /// `tenant` column (the attached-Postgres table is one physical
    /// relation shared by every replica of this deployment); `None` for
    /// the local table, whose tenancy is implicit (one DuckDB file per
    /// tenant, no `tenant` column at all).
    fn chat_tenant_scope(&self) -> Option<&str> {
        match self.chat_backend() {
            ChatBackend::Local => None,
            ChatBackend::AttachedPostgres { .. } => Some(self.tenant()),
        }
    }

    /// Append one message to a chat-group's history.
    ///
    /// Behaviour:
    ///
    /// - When `input.msg_id` is `None`, a ULID is generated server-side.
    /// - When `input.ts` is `None`, the row's `ts` is stamped with
    ///   DuckDB's `CURRENT_TIMESTAMP` at insert time. The resolved
    ///   timestamp is read back via `INSERT … RETURNING` so the caller
    ///   receives the exact value persisted.
    /// - When `input.embed` is `true`, the content is embedded inside
    ///   the per-tenant write lock (same ordering rationale as
    ///   `update_page`, see
    ///   docs/notes/discovered/2026-05-24-update-page-embed-order.md).
    /// - When `input.embed` is `false`, `dense_vec` is left NULL and
    ///   `embedded` is `FALSE`; similarity queries that target this
    ///   row must filter on `dense_vec IS NOT NULL`.
    pub async fn append_chat_message(
        &self,
        input: AppendChatMessage<'_>,
    ) -> Result<ChatMessage, IndexerError> {
        let msg_id: String = input
            .msg_id
            .map(str::to_owned)
            .unwrap_or_else(|| Ulid::new().to_string());

        let metadata_json: Option<String> = match &input.metadata {
            Some(v) => Some(serde_json::to_string(v)?),
            None => None,
        };

        // Acquire the per-tenant DuckDB lock BEFORE embedding —
        // mirrors update_page's ordering so concurrent appends
        // serialise on the (embed → write) sequence and a slower
        // embed cannot overwrite a newer commit.
        let conn = self.conn.lock().await;

        let dense_vec_literal: Option<String> = if input.embed {
            let vectors = self.embedder.embed(&[input.content]).await?;
            let v0 = vectors.into_iter().next().ok_or_else(|| {
                IndexerError::Embed(EmbedError::Backend(
                    "embedder returned no vectors for a single-text batch".to_owned(),
                ))
            })?;
            if v0.len() != BLOCKS_DENSE_VEC_DIM {
                return Err(IndexerError::EmbedderDimMismatch {
                    expected: BLOCKS_DENSE_VEC_DIM,
                    got: v0.len(),
                });
            }
            Some(format!(
                "{}::FLOAT[{}]",
                format_vector_literal(&v0),
                BLOCKS_DENSE_VEC_DIM
            ))
        } else {
            None
        };

        // Vector literal is inlined (no string params; values come
        // from a trusted embedder), parameters cover all string
        // inputs. COALESCE lets a NULL `ts` parameter fall through
        // to CURRENT_TIMESTAMP without a second statement.
        //
        // The attached-Postgres table has no `metadata JSON` column
        // (JSON round-tripping through the DuckDB Postgres connector is
        // untested for this PR) — it stores the same JSON text as a
        // plain VARCHAR, so the `::JSON` cast is Local-only; the
        // `tenant` column is likewise AttachedPostgres-only.
        let table = self.chat_table();
        let vec_expr = dense_vec_literal.as_deref().unwrap_or("NULL");
        let mut bindings: Vec<Box<dyn duckdb::ToSql + Send>> = Vec::new();
        // `Some(ts)` when `ts` was already resolved (and the INSERT ran
        // with no `RETURNING`); `None` when the INSERT itself resolves
        // and returns it (the `RETURNING` path, Local only — see below).
        let mut already_resolved_ts: Option<String> = None;
        let sql = match self.chat_tenant_scope() {
            None => {
                bindings.push(Box::new(input.chat_group_id.to_owned()));
                bindings.push(Box::new(msg_id.clone()));
                bindings.push(Box::new(input.ts.map(str::to_owned)));
                bindings.push(Box::new(input.role.to_owned()));
                bindings.push(Box::new(input.author.map(str::to_owned)));
                bindings.push(Box::new(input.content.to_owned()));
                bindings.push(Box::new(metadata_json.clone()));
                bindings.push(Box::new(input.embed));
                format!(
                    "INSERT INTO {table} \
                     (chat_group_id, msg_id, ts, role, author, content, metadata, dense_vec, embedded) \
                     VALUES (?, ?, \
                             COALESCE(TRY_CAST(? AS TIMESTAMP), CURRENT_TIMESTAMP), \
                             ?, ?, ?, ?::JSON, {vec_expr}, ?) \
                     RETURNING strftime(ts, '%Y-%m-%dT%H:%M:%SZ')"
                )
            }
            Some(tenant) => {
                // The DuckDB Postgres connector rejects `RETURNING` on an
                // insert into an attached Postgres table ("Binder Error:
                // RETURNING clause not yet supported for insertion into
                // Postgres table" — probed empirically against a live
                // container). Resolve + format `ts` FIRST with a plain
                // scalar `SELECT` (touches no table, so it's an ordinary
                // DuckDB query, not an attached-table insert), then bind
                // the resolved, already-RFC-3339 value straight into the
                // INSERT instead of leaning on COALESCE/RETURNING there.
                let resolved_ts: String = conn.query_row(
                    "SELECT strftime(COALESCE(TRY_CAST(? AS TIMESTAMP), CURRENT_TIMESTAMP), \
                     '%Y-%m-%dT%H:%M:%SZ')",
                    [input.ts],
                    |row| row.get(0),
                )?;
                already_resolved_ts = Some(resolved_ts.clone());
                bindings.push(Box::new(tenant.to_owned()));
                bindings.push(Box::new(input.chat_group_id.to_owned()));
                bindings.push(Box::new(msg_id.clone()));
                bindings.push(Box::new(resolved_ts));
                bindings.push(Box::new(input.role.to_owned()));
                bindings.push(Box::new(input.author.map(str::to_owned)));
                bindings.push(Box::new(input.content.to_owned()));
                bindings.push(Box::new(metadata_json.clone()));
                bindings.push(Box::new(input.embed));
                format!(
                    "INSERT INTO {table} \
                     (tenant, chat_group_id, msg_id, ts, role, author, content, metadata, dense_vec, embedded) \
                     VALUES (?, ?, ?, TRY_CAST(? AS TIMESTAMP), ?, ?, ?, ?, {vec_expr}, ?)"
                )
            }
        };

        let param_refs: Vec<&dyn duckdb::ToSql> = bindings
            .iter()
            .map(|b| b.as_ref() as &dyn duckdb::ToSql)
            .collect();
        let stored_ts: String = match already_resolved_ts {
            Some(ts) => {
                conn.execute(&sql, param_refs.as_slice())?;
                ts
            }
            None => conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))?,
        };

        Ok(ChatMessage {
            chat_group_id: input.chat_group_id.to_owned(),
            msg_id,
            ts: stored_ts,
            role: input.role.to_owned(),
            author: input.author.map(str::to_owned),
            content: input.content.to_owned(),
            metadata: input.metadata,
            embedded: input.embed,
        })
    }

    /// Return a page of messages for the chat group, time-ordered.
    ///
    /// `since` is inclusive, `until` is exclusive. `limit` is clamped
    /// to `[1, ChatPage::MAX_LIMIT]`. If more rows remain past the
    /// returned page, `next_cursor` is populated with an opaque
    /// base64-encoded `(ts, msg_id)` pair; the caller passes it back
    /// verbatim in the next call to continue.
    pub async fn list_chat_messages(
        &self,
        input: ListChatMessages<'_>,
    ) -> Result<ChatPage, IndexerError> {
        let limit = input.limit.clamp(1, ChatPage::MAX_LIMIT);

        let cursor = match input.cursor {
            Some(raw) => Some(Cursor::decode(raw)?),
            None => None,
        };

        // Build the WHERE clause. The placeholders fill in
        // chat_group_id (always), since/until (optional), cursor
        // (optional, comparison direction depends on order). We
        // request `limit + 1` rows so a present `next_cursor` is
        // detectable without a second query.
        let order = match input.direction {
            OrderDir::Asc => "ASC",
            OrderDir::Desc => "DESC",
        };
        let cursor_cmp = match input.direction {
            OrderDir::Asc => ">",
            OrderDir::Desc => "<",
        };

        let mut where_clauses = vec!["chat_group_id = ?".to_owned()];
        let mut bindings: Vec<Box<dyn duckdb::ToSql + Send>> =
            vec![Box::new(input.chat_group_id.to_owned())];
        if let Some(tenant) = self.chat_tenant_scope() {
            where_clauses.push("tenant = ?".to_owned());
            bindings.push(Box::new(tenant.to_owned()));
        }

        if let Some(since) = input.since {
            where_clauses.push("ts >= TRY_CAST(? AS TIMESTAMP)".to_owned());
            bindings.push(Box::new(since.to_owned()));
        }
        if let Some(until) = input.until {
            where_clauses.push("ts <  TRY_CAST(? AS TIMESTAMP)".to_owned());
            bindings.push(Box::new(until.to_owned()));
        }
        if let Some(c) = &cursor {
            where_clauses.push(format!(
                "(ts, msg_id) {cursor_cmp} (TRY_CAST(? AS TIMESTAMP), ?)"
            ));
            bindings.push(Box::new(c.ts.clone()));
            bindings.push(Box::new(c.msg_id.clone()));
        }

        let table = self.chat_table();
        let sql = format!(
            "SELECT chat_group_id, msg_id, \
                    strftime(ts, '%Y-%m-%dT%H:%M:%SZ'), \
                    role, author, content, metadata::VARCHAR, embedded \
             FROM {table} \
             WHERE {where_clause} \
             ORDER BY ts {order}, msg_id {order} \
             LIMIT ?",
            where_clause = where_clauses.join(" AND "),
        );
        bindings.push(Box::new((limit + 1) as i64));

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn duckdb::ToSql> = bindings
            .iter()
            .map(|b| b.as_ref() as &dyn duckdb::ToSql)
            .collect();
        let mut rows = stmt.query(param_refs.as_slice())?;

        let mut messages = Vec::with_capacity(limit + 1);
        while let Some(row) = rows.next()? {
            let chat_group_id: String = row.get(0)?;
            let msg_id: String = row.get(1)?;
            let ts: String = row.get(2)?;
            let role: String = row.get(3)?;
            let author: Option<String> = row.get(4)?;
            let content: String = row.get(5)?;
            let metadata_json: Option<String> = row.get(6)?;
            let embedded: bool = row.get(7)?;
            let metadata: Option<serde_json::Value> = match metadata_json {
                Some(s) => Some(serde_json::from_str(&s)?),
                None => None,
            };
            messages.push(ChatMessage {
                chat_group_id,
                msg_id,
                ts,
                role,
                author,
                content,
                metadata,
                embedded,
            });
        }

        // We asked for one extra row; if it's there, pop it and emit
        // a cursor pointing at the last row of the visible page.
        let next_cursor = if messages.len() > limit {
            messages.truncate(limit);
            messages.last().map(|m| {
                Cursor {
                    ts: m.ts.clone(),
                    msg_id: m.msg_id.clone(),
                }
                .encode()
            })
        } else {
            None
        };

        Ok(ChatPage {
            messages,
            next_cursor,
        })
    }

    /// Delete chat history matching the optional filters. Returns
    /// the number of rows removed. Used for both retention pruning
    /// (`Some(before_ts)`) and GDPR-style erasure (`Some(chat_group_id)`
    /// for a whole group, `Some(author)` for a single member).
    ///
    /// The three filters compose with AND; any left `None` is not
    /// constrained. Notable points:
    /// - all `None` removes the whole `chat_messages` table for this
    ///   tenant.
    /// - `chat_group_id = Some(g)` alone removes every message in `g`.
    /// - `author = Some(a)` alone removes member `a`'s messages across
    ///   every group (GDPR right-to-erasure of one member).
    /// - `chat_group_id = Some(g), author = Some(a)` removes member
    ///   `a`'s messages within group `g` only.
    /// - `before_ts = Some(t)` further restricts to `ts < t` (strict).
    ///
    /// The MCP surface exposes this only via the admin RPC; the
    /// agent-side tools never call it.
    pub async fn delete_chat_history(
        &self,
        chat_group_id: Option<&str>,
        before_ts: Option<&str>,
        author: Option<&str>,
    ) -> Result<usize, IndexerError> {
        let mut where_clauses: Vec<&str> = Vec::new();
        let mut bindings: Vec<Box<dyn duckdb::ToSql + Send>> = Vec::new();

        // Scoped to this tenant on the shared attached-Postgres table
        // (DuckLake PR 8) — "all `None`" must still never delete another
        // tenant's history sharing the same physical relation.
        if let Some(tenant) = self.chat_tenant_scope() {
            where_clauses.push("tenant = ?");
            bindings.push(Box::new(tenant.to_owned()));
        }
        if let Some(g) = chat_group_id {
            where_clauses.push("chat_group_id = ?");
            bindings.push(Box::new(g.to_owned()));
        }
        if let Some(a) = author {
            where_clauses.push("author = ?");
            bindings.push(Box::new(a.to_owned()));
        }
        if let Some(ts) = before_ts {
            where_clauses.push("ts < TRY_CAST(? AS TIMESTAMP)");
            bindings.push(Box::new(ts.to_owned()));
        }

        let table = self.chat_table();
        let sql = if where_clauses.is_empty() {
            format!("DELETE FROM {table}")
        } else {
            format!("DELETE FROM {table} WHERE {}", where_clauses.join(" AND "))
        };

        let conn = self.conn.lock().await;
        let param_refs: Vec<&dyn duckdb::ToSql> = bindings
            .iter()
            .map(|b| b.as_ref() as &dyn duckdb::ToSql)
            .collect();
        let n = conn.execute(&sql, param_refs.as_slice())?;
        Ok(n)
    }

    /// Embedding-similarity search over one chat group's embedded
    /// messages (`dense_vec IS NOT NULL`), nearest first.
    ///
    /// The local `chat_messages` table carries an HNSW index
    /// (`hnsw_chat_vec`) but a chat group's history is small enough that
    /// a plain `ORDER BY array_cosine_distance(...) LIMIT n` scan is fine
    /// without it — this is also the ONLY option over the attached-
    /// Postgres table (DuckLake PR 8), which has no HNSW at all (the
    /// `vss` index type does not exist on an attached Postgres relation).
    /// `dense_vec` there is `FLOAT[]` (list, not the fixed-width
    /// `FLOAT[768]` array Postgres attach cannot store — same lesson as
    /// the lake's own blocks table), so the query casts it back
    /// `::FLOAT[768]` before computing the distance; the local table is
    /// already `FLOAT[768]` and needs no cast.
    pub async fn search_chat_messages(
        &self,
        input: SearchChatMessages<'_>,
    ) -> Result<Vec<ChatMessage>, IndexerError> {
        let vectors = self.embedder.embed(&[input.query]).await?;
        let v0 = vectors.into_iter().next().ok_or_else(|| {
            IndexerError::Embed(EmbedError::Backend(
                "embedder returned no vectors for a single-text batch".to_owned(),
            ))
        })?;
        if v0.len() != BLOCKS_DENSE_VEC_DIM {
            return Err(IndexerError::EmbedderDimMismatch {
                expected: BLOCKS_DENSE_VEC_DIM,
                got: v0.len(),
            });
        }
        let q_lit = format!(
            "{}::FLOAT[{BLOCKS_DENSE_VEC_DIM}]",
            format_vector_literal(&v0)
        );
        let dense_vec_expr = match self.chat_backend() {
            ChatBackend::Local => "dense_vec".to_owned(),
            ChatBackend::AttachedPostgres { .. } => {
                format!("dense_vec::FLOAT[{BLOCKS_DENSE_VEC_DIM}]")
            }
        };

        let mut where_clauses = vec![
            "chat_group_id = ?".to_owned(),
            "dense_vec IS NOT NULL".to_owned(),
        ];
        let mut bindings: Vec<Box<dyn duckdb::ToSql + Send>> =
            vec![Box::new(input.chat_group_id.to_owned())];
        if let Some(tenant) = self.chat_tenant_scope() {
            where_clauses.push("tenant = ?".to_owned());
            bindings.push(Box::new(tenant.to_owned()));
        }
        bindings.push(Box::new(input.limit as i64));

        let table = self.chat_table();
        let sql = format!(
            "SELECT chat_group_id, msg_id, \
                    strftime(ts, '%Y-%m-%dT%H:%M:%SZ'), \
                    role, author, content, metadata::VARCHAR, embedded \
             FROM {table} \
             WHERE {where_clause} \
             ORDER BY array_cosine_distance({dense_vec_expr}, {q_lit}) ASC \
             LIMIT ?",
            where_clause = where_clauses.join(" AND "),
        );

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn duckdb::ToSql> = bindings
            .iter()
            .map(|b| b.as_ref() as &dyn duckdb::ToSql)
            .collect();
        let mut rows = stmt.query(param_refs.as_slice())?;

        let mut messages = Vec::new();
        while let Some(row) = rows.next()? {
            let metadata_json: Option<String> = row.get(6)?;
            messages.push(ChatMessage {
                chat_group_id: row.get(0)?,
                msg_id: row.get(1)?,
                ts: row.get(2)?,
                role: row.get(3)?,
                author: row.get(4)?,
                content: row.get(5)?,
                metadata: match metadata_json {
                    Some(s) => Some(serde_json::from_str(&s)?),
                    None => None,
                },
                embedded: row.get(7)?,
            });
        }
        Ok(messages)
    }
}
