//! Claim leases on inbox events (#818): which runner takes an event.
//!
//! Several runners on one tenant all read the same inbox. Each one's ledger
//! is still its LOCAL idempotency authority; the lease decides which runner
//! may start: `claim_event` is a compare-and-set on one row per event, held by
//! the runner's identity until it lapses, is renewed, or the event is
//! assigned (`assign_event` releases it).
//!
//! The table lives in a gateway-local, in-memory DuckDB, not in the index:
//! a lease is ephemeral coordination, not tenant data, so it must not become
//! a lake snapshot (the churn behind the heron GC hang) and it works the same
//! over a single file, DuckLake or attached Postgres. A gateway restart
//! forgets every lease; runners re-take theirs on the next renew, and the
//! ledger still stops a runner from starting an event twice. Claims are
//! writes, so a read replica refuses them and they reach the one writer.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use duckdb::{Connection, params};

/// A lease as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lease {
    pub holder: String,
    /// Unix milliseconds.
    pub expires_ms: i64,
}

/// What a claim or a renew found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimOutcome {
    /// The caller holds the lease now.
    pub won: bool,
    /// The lease as it stands after the call.
    pub lease: Lease,
}

/// How long a lapsed lease is kept before a write sweeps it.
const SWEEP_SLACK_MS: i64 = 10 * 60 * 1000;

pub(crate) struct EventClaims {
    conn: Mutex<Connection>,
}

/// Unix milliseconds now.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl EventClaims {
    /// An empty, in-memory claims table.
    ///
    /// # Errors
    /// When DuckDB cannot open an in-memory database.
    pub(crate) fn open() -> Result<Self, duckdb::Error> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE event_claims (
                 event_id   VARCHAR PRIMARY KEY,
                 holder     VARCHAR NOT NULL,
                 expires_ms BIGINT  NOT NULL
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Take `event_id` for `holder` unless someone else holds a live lease.
    /// A holder re-claiming its own lease extends it.
    pub(crate) fn claim(
        &self,
        event_id: &str,
        holder: &str,
        lease_ms: i64,
        now: i64,
    ) -> Result<ClaimOutcome, duckdb::Error> {
        self.compare_and_set(event_id, holder, lease_ms, now, true)
    }

    /// Extend `holder`'s lease. Never takes over another holder's lease, even
    /// a lapsed one (that is [`Self::claim`]'s job); a lease no row records
    /// any more (a gateway restart, a sweep) is re-taken.
    pub(crate) fn renew(
        &self,
        event_id: &str,
        holder: &str,
        lease_ms: i64,
        now: i64,
    ) -> Result<ClaimOutcome, duckdb::Error> {
        self.compare_and_set(event_id, holder, lease_ms, now, false)
    }

    /// The live lease on `event_id`, if any.
    pub(crate) fn live(&self, event_id: &str, now: i64) -> Result<Option<Lease>, duckdb::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(read(&conn, event_id)?.filter(|l| l.expires_ms > now))
    }

    /// Drop the lease on `event_id` (it was assigned: nothing left to claim).
    pub(crate) fn release(&self, event_id: &str) -> Result<(), duckdb::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("DELETE FROM event_claims WHERE event_id = ?", [event_id])?;
        Ok(())
    }

    /// The CAS. The connection mutex makes read-decide-write one step, which
    /// is exactly what N concurrent claims need: one of them wins.
    fn compare_and_set(
        &self,
        event_id: &str,
        holder: &str,
        lease_ms: i64,
        now: i64,
        take_lapsed: bool,
    ) -> Result<ClaimOutcome, duckdb::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = read(&conn, event_id)?
            && current.holder != holder
            && (current.expires_ms > now || !take_lapsed)
        {
            return Ok(ClaimOutcome {
                won: false,
                lease: current,
            });
        }
        let lease = Lease {
            holder: holder.to_owned(),
            expires_ms: now.saturating_add(lease_ms),
        };
        conn.execute(
            "INSERT OR REPLACE INTO event_claims (event_id, holder, expires_ms) VALUES (?, ?, ?)",
            params![event_id, lease.holder, lease.expires_ms],
        )?;
        // Leases lapsed for a while are not worth keeping; sweeping on write
        // keeps the table near the size of the live set. The slack leaves a
        // just-lapsed lease to its holder's renew rather than to a sweep.
        conn.execute(
            "DELETE FROM event_claims WHERE expires_ms <= ?",
            [now.saturating_sub(SWEEP_SLACK_MS)],
        )?;
        Ok(ClaimOutcome { won: true, lease })
    }
}

fn read(conn: &Connection, event_id: &str) -> Result<Option<Lease>, duckdb::Error> {
    let mut stmt =
        conn.prepare_cached("SELECT holder, expires_ms FROM event_claims WHERE event_id = ?")?;
    let mut rows = stmt.query([event_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(Lease {
            holder: row.get(0)?,
            expires_ms: row.get(1)?,
        })),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time_until_the_lease_lapses() {
        let c = EventClaims::open().unwrap();
        assert!(c.claim("e1", "a", 1_000, 0).unwrap().won);
        let lost = c.claim("e1", "b", 1_000, 500).unwrap();
        assert!(!lost.won);
        assert_eq!(lost.lease.holder, "a");
        assert!(
            c.renew("e1", "a", 1_000, 900).unwrap().won,
            "the holder renews"
        );
        assert!(!c.renew("e1", "b", 1_000, 1_000).unwrap().won);
        assert!(
            !c.renew("e1", "b", 1_000, 1_950).unwrap().won,
            "renew never takes over"
        );
        assert!(
            c.claim("e1", "b", 1_000, 1_901).unwrap().won,
            "lapsed: claimable"
        );
        assert_eq!(c.live("e1", 1_902).unwrap().unwrap().holder, "b");
        c.release("e1").unwrap();
        assert_eq!(c.live("e1", 1_902).unwrap(), None);
        assert!(
            c.renew("e1", "a", 1_000, 2_000).unwrap().won,
            "a forgotten lease is re-taken"
        );
    }
}
