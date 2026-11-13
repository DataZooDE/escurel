//! The bridge's durable memory: one row per platform message ([`InboundTurn::key`]), in a local
//! DuckDB file. It is what makes a replayed activity a no-op and what lets a
//! restarted bridge finish the turns it had started.
//!
//! A turn moves `received` → `captured` → `replying` → `replied` (or
//! `expired`, or one of the failure closes: `unmapped`, `send_failed`,
//! `send_uncertain`). `replying` is claimed with a conditional update before
//! the reply is posted, so no two tasks — and no restart — can send a second
//! answer: a crash between the claim and the post loses that one reply rather
//! than risking a duplicate (the runner's own rule: no answer beats a wrong
//! one).

use std::path::Path;
use std::sync::Mutex;

use duckdb::{Connection, params};

use crate::channel::InboundTurn;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("state store: {0}")]
    Db(#[from] duckdb::Error),
    #[error("state store: a stored turn does not decode: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Where a turn stands. Stored as [`TurnState::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// Admitted; nothing is in escurel yet.
    Received,
    /// The user turn and its event are in escurel.
    Captured,
    /// The one reply is claimed and being posted.
    Replying,
    /// The answer was posted.
    Replied,
    /// No answer came in time; the timeout notice was posted.
    Expired,
    /// The principal template could not map the sender.
    Unmapped,
    /// The platform refused the reply, or never took it after retries.
    SendFailed,
    /// The reply may have been delivered (no success came back after the
    /// request left); not retried, so it is never posted twice.
    SendUncertain,
}

impl TurnState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Captured => "captured",
            Self::Replying => "replying",
            Self::Replied => "replied",
            Self::Expired => "expired",
            Self::Unmapped => "unmapped",
            Self::SendFailed => "send_failed",
            Self::SendUncertain => "send_uncertain",
        }
    }
}

/// A turn to (re)work, with what the store knows about it.
#[derive(Debug, Clone)]
pub struct OpenTurn {
    pub turn: InboundTurn,
    pub state: TurnState,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// # Errors
    /// The file cannot be opened or migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS turns (
                 channel     TEXT NOT NULL,
                 turn_key    TEXT NOT NULL,
                 turn        TEXT NOT NULL,
                 state       TEXT NOT NULL,
                 event_id    TEXT,
                 reply_key   TEXT,
                 created_at  TIMESTAMP NOT NULL DEFAULT current_timestamp,
                 updated_at  TIMESTAMP NOT NULL DEFAULT current_timestamp,
                 PRIMARY KEY (channel, turn_key)
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a new turn; `false` when this message was seen before.
    ///
    /// # Errors
    /// The write fails.
    pub fn admit(&self, channel: &str, turn: &InboundTurn) -> Result<bool, StoreError> {
        let json = serde_json::to_string(turn)?;
        let n = self.conn().execute(
            "INSERT INTO turns (channel, turn_key, turn, state) VALUES (?, ?, ?, 'received')
             ON CONFLICT DO NOTHING",
            params![channel, turn.key(), json],
        )?;
        Ok(n == 1)
    }

    /// The turn is in escurel: its event exists.
    ///
    /// # Errors
    /// The write fails.
    pub fn captured(
        &self,
        channel: &str,
        turn_key: &str,
        event_id: &str,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE turns SET state = 'captured', event_id = ?, updated_at = current_timestamp
             WHERE channel = ? AND turn_key = ? AND state = 'received'",
            params![event_id, channel, turn_key],
        )?;
        Ok(())
    }

    /// Claim the right to send the one reply, keyed by `reply_key`; `false`
    /// when a reply was already claimed (sent, or lost to a crash).
    ///
    /// # Errors
    /// The write fails.
    pub fn claim_reply(
        &self,
        channel: &str,
        turn_key: &str,
        reply_key: &str,
    ) -> Result<bool, StoreError> {
        let n = self.conn().execute(
            "UPDATE turns SET state = 'replying', reply_key = ?, updated_at = current_timestamp
             WHERE channel = ? AND turn_key = ? AND state IN ('received', 'captured')",
            params![reply_key, channel, turn_key],
        )?;
        Ok(n == 1)
    }

    /// Close a turn in its final `state`.
    ///
    /// # Errors
    /// The write fails.
    pub fn close(&self, channel: &str, turn_key: &str, state: TurnState) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE turns SET state = ?, updated_at = current_timestamp
             WHERE channel = ? AND turn_key = ?",
            params![state.as_str(), channel, turn_key],
        )?;
        Ok(())
    }

    /// The turns a previous process left unanswered.
    ///
    /// # Errors
    /// The read fails or a row does not decode.
    pub fn open_turns(&self, channel: &str) -> Result<Vec<OpenTurn>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT turn, state FROM turns
             WHERE channel = ? AND state IN ('received', 'captured') ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![channel], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (json, state) = row?;
            out.push(OpenTurn {
                turn: serde_json::from_str(&json)?,
                state: if state == TurnState::Captured.as_str() {
                    TurnState::Captured
                } else {
                    TurnState::Received
                },
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::UserRef;

    fn turn(id: &str) -> InboundTurn {
        InboundTurn {
            message_id: id.to_owned(),
            conversation_id: "c".into(),
            user: UserRef {
                id: "u".into(),
                directory_id: None,
                directory_tenant: None,
                display_name: None,
            },
            text: "hi".into(),
            attachments: vec![],
            reply_to: serde_json::Value::Null,
        }
    }

    #[test]
    fn a_message_is_admitted_once_and_replied_to_once_across_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.duckdb");
        {
            let s = Store::open(&path).unwrap();
            assert!(s.admit("msteams", &turn("1")).unwrap());
            assert!(!s.admit("msteams", &turn("1")).unwrap());
            s.captured("msteams", &turn("1").key(), "ev-1").unwrap();
        }
        let s = Store::open(&path).unwrap();
        let open = s.open_turns("msteams").unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].state, TurnState::Captured);
        let key = turn("1").key();
        assert!(s.claim_reply("msteams", &key, "run:r:reply").unwrap());
        assert!(!s.claim_reply("msteams", &key, "run:r:reply").unwrap());
        s.close("msteams", &key, TurnState::Replied).unwrap();
        assert!(s.open_turns("msteams").unwrap().is_empty());
    }
}
