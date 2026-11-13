//! Run transcripts (#822): the JSONL record of what a run's agent did, kept
//! as a content-addressed blob and tracked per run so retention deletes
//! transcript blobs and nothing else.

use std::time::SystemTime;

use bytes::Bytes;
use duckdb::params;
use escurel_storage::BlobId;

use crate::{Indexer, IndexerError};

/// A stored transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTranscript {
    pub blob_id: String,
    pub records: u64,
    pub bytes: u64,
}

fn timestamp(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t)
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string()
}

impl Indexer {
    /// Store `jsonl` as `run_id`'s transcript (`max_bytes` = the size cap).
    /// Idempotent: the blob is content-addressed, and a second upload for the
    /// same run replaces the first (whose blob goes when nothing else names
    /// it).
    ///
    /// # Errors
    /// When the blob store refuses the bytes (too large included) or the row
    /// cannot be written.
    pub async fn store_run_transcript(
        &self,
        run_id: &str,
        jsonl: Bytes,
        records: u64,
        max_bytes: u64,
    ) -> Result<StoredTranscript, IndexerError> {
        let bytes = jsonl.len() as u64;
        let id = self
            .store
            .put_blob(self.tenant(), jsonl, Some(max_bytes))
            .await?;
        let blob_id = id.as_str().to_owned();
        let replaced = {
            let conn = self.conn.lock().await;
            let previous: Option<String> = conn
                .query_row(
                    "SELECT blob_id FROM run_transcripts WHERE run_id = ?",
                    params![run_id],
                    |r| r.get(0),
                )
                .ok();
            conn.execute(
                "INSERT OR REPLACE INTO run_transcripts (run_id, blob_id, records, bytes, stored_at) \
                 VALUES (?, ?, ?, ?, CAST(? AS TIMESTAMP))",
                params![
                    run_id,
                    blob_id,
                    records as i64,
                    bytes as i64,
                    timestamp(SystemTime::now())
                ],
            )?;
            previous.filter(|p| *p != blob_id)
        };
        if let Some(old) = replaced {
            self.delete_unreferenced_transcript_blob(&old).await?;
        }
        Ok(StoredTranscript {
            blob_id,
            records,
            bytes,
        })
    }

    /// Delete every transcript stored before `cutoff`: its row, and its blob
    /// once no other run's transcript names the same bytes. Returns how many
    /// transcripts went.
    ///
    /// # Errors
    /// When the table cannot be read or written, or a blob delete fails.
    pub async fn sweep_run_transcripts(&self, cutoff: SystemTime) -> Result<u64, IndexerError> {
        let (blobs, swept) = {
            let conn = self.conn.lock().await;
            let cutoff = timestamp(cutoff);
            let mut stmt = conn.prepare(
                "SELECT DISTINCT blob_id FROM run_transcripts \
                 WHERE stored_at < CAST(? AS TIMESTAMP)",
            )?;
            let blobs = stmt
                .query_map(params![cutoff], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let n = conn.execute(
                "DELETE FROM run_transcripts WHERE stored_at < CAST(? AS TIMESTAMP)",
                params![cutoff],
            )?;
            (blobs, n as u64)
        };
        for blob in blobs {
            self.delete_unreferenced_transcript_blob(&blob).await?;
        }
        Ok(swept)
    }

    async fn delete_unreferenced_transcript_blob(&self, blob_id: &str) -> Result<(), IndexerError> {
        let still_named: i64 = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT count(*) FROM run_transcripts WHERE blob_id = ?",
                params![blob_id],
                |r| r.get(0),
            )?
        };
        if still_named > 0 {
            return Ok(());
        }
        let Some(id) = BlobId::parse(blob_id) else {
            return Ok(());
        };
        match self.store.delete_blob(self.tenant(), &id).await {
            Ok(()) | Err(escurel_storage::StoreError::NotFound(_)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
