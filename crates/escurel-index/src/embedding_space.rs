//! The embedding space is part of the index's identity.
//!
//! Vectors from two embedding models are not comparable: a cosine distance
//! between a `gemini-embedding-001` vector and an `alibaba:text-embedding-v4`
//! one is a number with no meaning, and an index holding both ranks garbage
//! with full confidence. Nothing in a vector says which model made it, so the
//! index records it — one `embedding_space` row, the embedder's opaque
//! [`Embedder::model_id`] and dimension — and
//! [`Indexer::reconcile_embedding_space_with`] brings the index into a target
//! embedder's space:
//!
//! | recorded | target | action |
//! |---|---|---|
//! | none, no vectors | any | record the target's space |
//! | same | same | re-embed only rows holding zero vectors (written while degraded) |
//! | other, or none over existing vectors | real model | **re-embed every block** |
//! | real model | `zero` (degraded) | keep vectors and record; semantic search is off |
//!
//! **A re-embed never truncates.** New vectors go to a staging table first,
//! committed per batch; only when every block has one are they swapped into
//! `blocks` — with the new record and the chat-vector reset — in ONE
//! transaction. A provider failure half way (quota, a rotated key) leaves the
//! old vectors and the old record serving, and the next attempt resumes from
//! the staged rows instead of paying for them again. The whole pass holds the
//! write lock, so no write lands in the old space between the last batch and
//! the swap; `activate` runs under that lock too, which is how
//! `embedding_reload` switches the query embedder at the same instant.

use escurel_embed::{EmbedError, Embedder};

use crate::chat::ChatBackend;
use crate::indexer::{BLOCKS_DENSE_VEC_DIM, Indexer, IndexerError, format_vector_literal};

/// The DDL of `embedding_space` and its staging table (also run by
/// `Migrator::ensure_embedding_space` at open; idempotent).
const EMBEDDING_SPACE_DDL: &str = include_str!("../sql/0021_embedding_space.sql");
/// The model id of the placeholder a degraded boot runs with.
const DEGRADED_MODEL_ID: &str = "zero";
/// Blocks embedded per provider request batch during a re-embed.
const REEMBED_BATCH: usize = 64;
/// The text a block's vector was computed from — exactly
/// `IndexChunk::embed_text` (`context \n body`, or `body`).
const EMBED_TEXT_SQL: &str =
    "CASE WHEN b.context IS NULL THEN b.body ELSE b.context || chr(10) || b.body END";
/// A row whose vector is absent or all zeros (a degraded write).
const ZERO_VECTOR_SQL: &str = "(b.dense_vec IS NULL OR \
     list_max(list_transform(b.dense_vec::FLOAT[], x -> abs(x))) = 0)";

/// What a reconcile did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingSpaceOutcome {
    /// Recorded the target's space over an index with no vectors yet.
    Recorded,
    /// Already in the target's space; nothing to re-embed.
    Unchanged,
    /// Same model, but blocks written while degraded held zero vectors;
    /// those were re-embedded.
    Repaired {
        /// How many blocks were re-embedded.
        blocks: u64,
    },
    /// The index was in another (or an unrecorded) space and was re-embedded.
    Reembedded {
        /// The model the old vectors came from; `None` when unrecorded.
        from: Option<String>,
        /// How many blocks were re-embedded.
        blocks: u64,
        /// Whether the chat vectors were cleared too. `false` when they live
        /// in a shared table (Postgres, the lake) that one replica must not
        /// rewrite — the caller says so in its log.
        chat_vectors_cleared: bool,
    },
    /// The target is the degraded placeholder; the recorded vectors were kept.
    KeptWhileDegraded {
        /// The model the kept vectors came from.
        recorded: String,
    },
}

impl Indexer {
    /// The recorded embedding space, `(model_id, dim)`.
    ///
    /// # Errors
    /// [`IndexerError::Duckdb`] on a query failure.
    pub async fn recorded_embedding_space(&self) -> Result<Option<(String, i64)>, IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute_batch(EMBEDDING_SPACE_DDL)?;
        let mut stmt = conn.prepare("SELECT model_id, dim FROM embedding_space")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// [`Self::reconcile_embedding_space_with`] for the indexer's own embedder
    /// (the boot path).
    ///
    /// # Errors
    /// As [`Self::reconcile_embedding_space_with`].
    pub async fn reconcile_embedding_space(&self) -> Result<EmbeddingSpaceOutcome, IndexerError> {
        let target = std::sync::Arc::clone(&self.embedder);
        self.reconcile_embedding_space_with(target.as_ref(), || {})
            .await
    }

    /// Bring the index into `target`'s space (see the module docs), then run
    /// `activate` under the write lock. On error nothing is activated and the
    /// index keeps serving its recorded space.
    ///
    /// # Errors
    /// [`IndexerError::EmbedderDimMismatch`] before anything is touched when
    /// `target` cannot fit the `FLOAT[768]` columns; the provider's error when
    /// a re-embed batch fails (the staged batches are kept for the next try).
    pub async fn reconcile_embedding_space_with<F>(
        &self,
        target: &dyn Embedder,
        activate: F,
    ) -> Result<EmbeddingSpaceOutcome, IndexerError>
    where
        F: FnOnce() + Send,
    {
        if target.dim() != BLOCKS_DENSE_VEC_DIM {
            return Err(IndexerError::EmbedderDimMismatch {
                expected: BLOCKS_DENSE_VEC_DIM,
                got: target.dim(),
            });
        }
        let _write = self.write_guard().await;
        let live = target.model_id();
        let recorded = self.recorded_embedding_space().await?.map(|(m, _)| m);
        let outcome = if live == DEGRADED_MODEL_ID {
            // A degraded target never rewrites vectors: it would replace good
            // ones with nothing. An unrecorded index is recorded as `zero`, so
            // the first real model re-embeds it.
            match recorded {
                Some(model) if model != DEGRADED_MODEL_ID => {
                    self.activate_now(activate).await;
                    EmbeddingSpaceOutcome::KeptWhileDegraded { recorded: model }
                }
                Some(_) => {
                    self.activate_now(activate).await;
                    EmbeddingSpaceOutcome::Unchanged
                }
                None => {
                    self.record_space(&live, activate).await?;
                    EmbeddingSpaceOutcome::Recorded
                }
            }
        } else if recorded.as_deref() == Some(live.as_str()) {
            match self.reembed(target, &live, true, false, activate).await? {
                0 => EmbeddingSpaceOutcome::Unchanged,
                blocks => EmbeddingSpaceOutcome::Repaired { blocks },
            }
        } else if recorded.is_none() && !self.has_vectors().await? {
            self.record_space(&live, activate).await?;
            EmbeddingSpaceOutcome::Recorded
        } else {
            // Another model's vectors, or vectors of unknown provenance (an
            // index from before this record existed): re-embed, never assume.
            let clear_chat = matches!(self.chat_backend(), ChatBackend::Local);
            let blocks = self
                .reembed(target, &live, false, clear_chat, activate)
                .await?;
            EmbeddingSpaceOutcome::Reembedded {
                from: recorded,
                blocks,
                chat_vectors_cleared: clear_chat,
            }
        };
        Ok(outcome)
    }

    /// Run `activate` under the connection mutex (see `reembed`).
    async fn activate_now<F: FnOnce() + Send>(&self, activate: F) {
        let _conn = self.conn.lock().await;
        activate();
    }

    async fn has_vectors(&self) -> Result<bool, IndexerError> {
        let conn = self.conn.lock().await;
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM blocks WHERE dense_vec IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    async fn record_space<F: FnOnce() + Send>(
        &self,
        model_id: &str,
        activate: F,
    ) -> Result<(), IndexerError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO embedding_space (singleton, model_id, dim) \
             VALUES (TRUE, ?, ?)",
            duckdb::params![model_id, BLOCKS_DENSE_VEC_DIM as i64],
        )?;
        activate();
        Ok(())
    }

    /// Re-embed blocks into `live`'s space through the staging table, then
    /// swap them in, record the space and (when asked) clear local chat
    /// vectors in one transaction. `only_zero` limits the pass to blocks with
    /// zero or missing vectors. Returns how many blocks were swapped. The
    /// caller holds the write lock.
    async fn reembed<F: FnOnce() + Send>(
        &self,
        target: &dyn Embedder,
        live: &str,
        only_zero: bool,
        clear_chat: bool,
        activate: F,
    ) -> Result<u64, IndexerError> {
        {
            let conn = self.conn.lock().await;
            conn.execute_batch(EMBEDDING_SPACE_DDL)?;
            // Staged rows are only worth keeping for THIS target and for the
            // text they were computed from; anything else is stale.
            conn.execute("DELETE FROM embedding_reembed WHERE model_id <> ?", [live])?;
            conn.execute_batch(&format!(
                "DELETE FROM embedding_reembed s WHERE NOT EXISTS \
                   (SELECT 1 FROM blocks b WHERE b.block_id = s.block_id \
                    AND md5({EMBED_TEXT_SQL}) = s.text_md5);"
            ))?;
        }
        let filter = if only_zero { ZERO_VECTOR_SQL } else { "TRUE" };
        loop {
            let batch: Vec<(String, String)> = {
                let conn = self.conn.lock().await;
                let mut stmt = conn.prepare(&format!(
                    "SELECT b.block_id, {EMBED_TEXT_SQL} FROM blocks b \
                     WHERE {filter} AND NOT EXISTS \
                       (SELECT 1 FROM embedding_reembed s WHERE s.block_id = b.block_id) \
                     ORDER BY b.block_id LIMIT {REEMBED_BATCH}"
                ))?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?
            };
            if batch.is_empty() {
                break;
            }
            // Off the connection mutex: reads keep flowing while the provider
            // answers. The write lock keeps the corpus still.
            let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();
            let vectors = target.embed(&texts).await?;
            if vectors.len() != batch.len() {
                return Err(IndexerError::Embed(EmbedError::Backend(format!(
                    "embedder returned {} vectors for {} texts",
                    vectors.len(),
                    batch.len()
                ))));
            }
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            for ((block_id, text), v) in batch.iter().zip(&vectors) {
                if v.len() != BLOCKS_DENSE_VEC_DIM {
                    return Err(IndexerError::EmbedderDimMismatch {
                        expected: BLOCKS_DENSE_VEC_DIM,
                        got: v.len(),
                    });
                }
                tx.execute(
                    &format!(
                        "INSERT INTO embedding_reembed (block_id, model_id, text_md5, dense_vec) \
                         VALUES (?, ?, md5(?), {}::FLOAT[{BLOCKS_DENSE_VEC_DIM}])",
                        format_vector_literal(v)
                    ),
                    duckdb::params![block_id, live, text],
                )?;
            }
            tx.commit()?;
        }
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let swapped = tx.execute(
            "UPDATE blocks SET dense_vec = s.dense_vec FROM embedding_reembed s \
             WHERE blocks.block_id = s.block_id",
            [],
        )?;
        tx.execute("DELETE FROM embedding_reembed", [])?;
        tx.execute(
            "INSERT OR REPLACE INTO embedding_space (singleton, model_id, dim) \
             VALUES (TRUE, ?, ?)",
            duckdb::params![live, BLOCKS_DENSE_VEC_DIM as i64],
        )?;
        if clear_chat {
            // Chat messages are not in the markdown lane, so they are not
            // re-derived; a vector-less row is skipped by semantic chat
            // search, a wrong-space one would be misranked.
            tx.execute(
                "UPDATE chat_messages SET dense_vec = NULL, embedded = FALSE \
                 WHERE dense_vec IS NOT NULL",
                [],
            )?;
        }
        tx.commit()?;
        // Activated while the connection is still held: a chat append (which
        // takes only the connection, not the write lock) cannot slip an
        // old-model vector in between the swap and the switch.
        activate();
        drop(conn);
        if swapped > 0 {
            self.bump_mutation_epoch();
        }
        Ok(swapped as u64)
    }
}
