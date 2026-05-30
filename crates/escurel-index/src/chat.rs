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
use duckdb::params;
use escurel_embed::EmbedError;
use ulid::Ulid;

use crate::indexer::{BLOCKS_DENSE_VEC_DIM, Indexer, IndexerError, format_vector_literal};
use crate::read::OrderDir;

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
        // to CURRENT_TIMESTAMP without a second statement; RETURNING
        // reads back the resolved value as RFC-3339 UTC.
        let vec_expr = dense_vec_literal.as_deref().unwrap_or("NULL");
        let sql = format!(
            "INSERT INTO chat_messages \
             (chat_group_id, msg_id, ts, role, author, content, metadata, dense_vec, embedded) \
             VALUES (?, ?, \
                     COALESCE(TRY_CAST(? AS TIMESTAMP), CURRENT_TIMESTAMP), \
                     ?, ?, ?, ?::JSON, {vec_expr}, ?) \
             RETURNING strftime(ts, '%Y-%m-%dT%H:%M:%SZ')"
        );

        let stored_ts: String = conn.query_row(
            &sql,
            params![
                input.chat_group_id,
                msg_id,
                input.ts,
                input.role,
                input.author,
                input.content,
                metadata_json,
                input.embed,
            ],
            |row| row.get(0),
        )?;

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

        let sql = format!(
            "SELECT chat_group_id, msg_id, \
                    strftime(ts, '%Y-%m-%dT%H:%M:%SZ'), \
                    role, author, content, metadata::VARCHAR, embedded \
             FROM chat_messages \
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

        let sql = if where_clauses.is_empty() {
            "DELETE FROM chat_messages".to_owned()
        } else {
            format!(
                "DELETE FROM chat_messages WHERE {}",
                where_clauses.join(" AND ")
            )
        };

        let conn = self.conn.lock().await;
        let param_refs: Vec<&dyn duckdb::ToSql> = bindings
            .iter()
            .map(|b| b.as_ref() as &dyn duckdb::ToSql)
            .collect();
        let n = conn.execute(&sql, param_refs.as_slice())?;
        Ok(n)
    }
}
