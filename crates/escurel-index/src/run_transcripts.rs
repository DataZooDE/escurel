//! Run transcripts (#822): the JSONL record of what a run's agent did.
//!
//! KEY-addressed, one per run: `transcripts/<run_id>.jsonl` in the tenant's
//! lane store, never the content-addressed `blobs/` namespace. A document
//! blob and a transcript with identical bytes are therefore two objects, and
//! deleting a transcript (retention) can never delete a document. A run's
//! transcript is written once: an identical retry is a no-op, different
//! bytes for the same run are refused.
//!
//! `run_transcripts` (DuckDB) records when each was stored, for the
//! retention sweep, and its digest, for the write-once check. An index
//! rebuild forgets the rows but not the keys; the sweep adopts an
//! unregistered key as stored "now", so it expires one window later.

use std::time::SystemTime;

use bytes::Bytes;
use duckdb::{OptionalExt, params};
use escurel_storage::{Key, StoreError};
use sha2::{Digest, Sha256};

use crate::events::store_ts;
use crate::{Indexer, IndexerError};

/// Where transcripts live under a tenant.
const TRANSCRIPT_PREFIX: &str = "transcripts";

/// A stored transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTranscript {
    pub records: u64,
    pub bytes: u64,
    /// `sha256:<hex>` of the bytes.
    pub sha256: String,
}

fn ts(t: SystemTime) -> String {
    store_ts(&chrono::DateTime::<chrono::Utc>::from(t))
}

/// Whether `run_id` can name exactly one key: no path syntax.
fn usable_run_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && run_id != "."
        && !run_id.contains("..")
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

impl Indexer {
    fn transcript_key(&self, run_id: &str) -> Result<Key, IndexerError> {
        if !usable_run_id(run_id) {
            return Err(IndexerError::InvalidTranscriptRunId(run_id.to_owned()));
        }
        Ok(Key::new(
            self.tenant(),
            format!("{TRANSCRIPT_PREFIX}/{run_id}.jsonl"),
        )?)
    }

    /// Store `jsonl` as `run_id`'s transcript (`max_bytes` = the size cap).
    /// Written once per run: the same bytes again are a no-op, other bytes are
    /// [`IndexerError::TranscriptExists`].
    ///
    /// # Errors
    /// Over the cap, an unusable run id, a second different upload, or a store
    /// / table failure.
    pub async fn store_run_transcript(
        &self,
        run_id: &str,
        jsonl: Bytes,
        records: u64,
        max_bytes: u64,
    ) -> Result<StoredTranscript, IndexerError> {
        let key = self.transcript_key(run_id)?;
        let bytes = jsonl.len() as u64;
        if bytes > max_bytes {
            return Err(StoreError::BlobTooLarge {
                limit: max_bytes,
                actual: bytes,
            }
            .into());
        }
        let sha256 = format!("sha256:{:x}", Sha256::digest(&jsonl));
        let stored = StoredTranscript {
            records,
            bytes,
            sha256: sha256.clone(),
        };
        // Claim the run's row first (the conn lock serialises claims), so two different uploads for
        // one run cannot both reach the store.
        {
            let conn = self.conn.lock().await;
            let existing: Option<String> = conn
                .query_row(
                    "SELECT sha256 FROM run_transcripts WHERE run_id = ?",
                    params![run_id],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                Some(prev) if prev == sha256 => return Ok(stored),
                Some(_) => {
                    return Err(IndexerError::TranscriptExists {
                        run_id: run_id.to_owned(),
                    });
                }
                None => {}
            }
            // A key with no row is a transcript stored before an index rebuild: it is still
            // written-once.
            match self.store.read(&key).await {
                Ok(prev) if prev == jsonl => {}
                Ok(_) => {
                    return Err(IndexerError::TranscriptExists {
                        run_id: run_id.to_owned(),
                    });
                }
                Err(StoreError::NotFound(_)) => {}
                Err(e) => return Err(e.into()),
            }
            conn.execute(
                "INSERT INTO run_transcripts (run_id, sha256, records, bytes, stored_at) \
                 VALUES (?, ?, ?, ?, CAST(? AS TIMESTAMP))",
                params![
                    run_id,
                    sha256,
                    records as i64,
                    bytes as i64,
                    ts(SystemTime::now())
                ],
            )?;
        }
        if let Err(e) = self.store.write(&key, jsonl).await {
            // Release the claim so a retry can store it.
            let conn = self.conn.lock().await;
            let _ = conn.execute(
                "DELETE FROM run_transcripts WHERE run_id = ?",
                params![run_id],
            );
            return Err(e.into());
        }
        Ok(stored)
    }

    /// `run_id`'s transcript bytes, or `None` when none is stored (never
    /// written, or past its retention).
    ///
    /// # Errors
    /// An unusable run id, or a store failure other than absence.
    pub async fn read_run_transcript(&self, run_id: &str) -> Result<Option<Bytes>, IndexerError> {
        match self.store.read(&self.transcript_key(run_id)?).await {
            Ok(b) => Ok(Some(b)),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete every transcript stored before `cutoff` (its key and its row),
    /// then adopt any transcript key that has no row (an index rebuild's
    /// leftovers) as stored now. Only `transcripts/` keys are ever deleted.
    /// Returns how many transcripts went.
    ///
    /// # Errors
    /// When the table or the store cannot be read or written.
    pub async fn sweep_run_transcripts(&self, cutoff: SystemTime) -> Result<u64, IndexerError> {
        let expired: Vec<String> = {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(
                "SELECT run_id FROM run_transcripts WHERE stored_at < CAST(? AS TIMESTAMP)",
            )?;
            stmt.query_map(params![ts(cutoff)], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut swept = 0;
        for run_id in &expired {
            match self.store.delete(&self.transcript_key(run_id)?).await {
                Ok(()) | Err(StoreError::NotFound(_)) => {}
                Err(e) => return Err(e.into()),
            }
            let conn = self.conn.lock().await;
            conn.execute(
                "DELETE FROM run_transcripts WHERE run_id = ?",
                params![run_id],
            )?;
            swept += 1;
        }
        self.adopt_unregistered_transcripts().await?;
        Ok(swept)
    }

    async fn adopt_unregistered_transcripts(&self) -> Result<(), IndexerError> {
        let prefix = Key::new(self.tenant(), TRANSCRIPT_PREFIX)?;
        let keys = self.store.list(&prefix).await?;
        let now = ts(SystemTime::now());
        let conn = self.conn.lock().await;
        for key in keys {
            let Some(run_id) = key
                .path()
                .strip_prefix("transcripts/")
                .and_then(|n| n.strip_suffix(".jsonl"))
                .filter(|r| usable_run_id(r))
            else {
                continue;
            };
            // Size and digest are unknown without reading the bytes; the row exists for retention.
            conn.execute(
                "INSERT INTO run_transcripts (run_id, sha256, records, bytes, stored_at) \
                 VALUES (?, '', 0, 0, CAST(? AS TIMESTAMP)) ON CONFLICT DO NOTHING",
                params![run_id, now],
            )?;
        }
        Ok(())
    }
}
