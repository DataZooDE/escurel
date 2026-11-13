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
//! forgets every lease, so a restarted gateway starts in a BOOT GRACE: for
//! the longest lease it may have issued, an event captured before it started
//! (by the store's `seq`, not the caller's `at`) that has no row cannot be
//! claimed — it answers `claimed: false`, holder [`RECOVERING`] — and only
//! `renew_claim` re-creates its row. A runner that was running it renews (it
//! renews at half its lease), and nobody else starts it meanwhile. Events
//! captured after the restart are claimable at once. Claims are writes, so a
//! read replica refuses them and they reach the one writer.

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

/// The holder a claim reports while the boot grace withholds an event.
pub(crate) const RECOVERING: &str = "(recovering)";

/// The holder of the tombstone an assigned event leaves behind: a claim that
/// read the event as unprocessed just before the assign must still lose.
pub(crate) const PROCESSED: &str = "(processed)";

/// How long an assign's tombstone outlives it: far past any claim that could
/// have read the event before it was assigned.
const TOMBSTONE_MS: i64 = 60 * 60 * 1000;

/// The window after a restart in which leases the gateway forgot may still be
/// held by a runner.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BootGrace {
    /// Unix milliseconds the grace ends.
    pub until_ms: i64,
    /// The highest event `seq` at boot: events at or below it may be held.
    pub watermark_seq: i64,
}

pub(crate) struct EventClaims {
    conn: Mutex<Connection>,
    grace: Option<BootGrace>,
}

/// Unix milliseconds now.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl EventClaims {
    /// An empty, in-memory claims table, in `grace` after a (re)start.
    ///
    /// # Errors
    /// When DuckDB cannot open an in-memory database.
    pub(crate) fn open(grace: Option<BootGrace>) -> Result<Self, duckdb::Error> {
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
            grace,
        })
    }

    /// Whether the boot grace is still on at `now`.
    pub(crate) fn in_grace(&self, now: i64) -> bool {
        self.grace.is_some_and(|g| now < g.until_ms)
    }

    /// Take `event_id` for `holder` unless someone else holds a live lease.
    /// A holder re-claiming its own lease extends it. `seq` is the event's
    /// capture position: during the boot grace an event captured before the
    /// restart with no row is withheld (see the module docs).
    pub(crate) fn claim(
        &self,
        event_id: &str,
        holder: &str,
        lease_ms: i64,
        now: i64,
        seq: Option<i64>,
    ) -> Result<ClaimOutcome, duckdb::Error> {
        let withheld = self
            .grace
            .filter(|g| now < g.until_ms && seq.is_none_or(|s| s <= g.watermark_seq));
        if let Some(g) = withheld {
            let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
            if read(&conn, event_id)?.is_none() {
                return Ok(ClaimOutcome {
                    won: false,
                    lease: Lease {
                        holder: RECOVERING.to_owned(),
                        expires_ms: g.until_ms,
                    },
                });
            }
        }
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

    /// The live lease on `event_id`, if any (an assign's tombstone is none).
    pub(crate) fn live(&self, event_id: &str, now: i64) -> Result<Option<Lease>, duckdb::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(read(&conn, event_id)?.filter(|l| l.expires_ms > now && l.holder != PROCESSED))
    }

    /// `event_id` was assigned: nothing left to claim. Its lease becomes a
    /// tombstone rather than disappearing (crew C-F5): `claim_event` reads
    /// the event's status before its compare-and-set, under another lock, so
    /// a claim that read "unprocessed" a moment before the assign would
    /// otherwise find no row and win.
    pub(crate) fn release(&self, event_id: &str, now: i64) -> Result<(), duckdb::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT OR REPLACE INTO event_claims (event_id, holder, expires_ms) VALUES (?, ?, ?)",
            params![event_id, PROCESSED, now.saturating_add(TOMBSTONE_MS)],
        )?;
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
            && (current.expires_ms > now || !take_lapsed || current.holder == PROCESSED)
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
        let c = EventClaims::open(None).unwrap();
        assert!(c.claim("e1", "a", 1_000, 0, None).unwrap().won);
        let lost = c.claim("e1", "b", 1_000, 500, None).unwrap();
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
            c.claim("e1", "b", 1_000, 1_901, None).unwrap().won,
            "lapsed: claimable"
        );
        assert_eq!(c.live("e1", 1_902).unwrap().unwrap().holder, "b");
        assert!(
            c.renew("e2", "a", 1_000, 2_000).unwrap().won,
            "a forgotten lease is re-taken"
        );
    }

    /// An assign leaves a tombstone (crew C-F5): a claim or renew that read
    /// the event as unprocessed just before cannot win it afterwards, even
    /// once the tombstone itself is long past any lease.
    #[test]
    fn an_assigned_event_cannot_be_claimed_or_renewed_again() {
        let c = EventClaims::open(None).unwrap();
        assert!(c.claim("e1", "a", 1_000, 0, None).unwrap().won);
        c.release("e1", 500).unwrap();
        assert_eq!(c.live("e1", 600).unwrap(), None, "no live lease to show");
        for (who, at) in [("a", 600), ("b", 600), ("b", 500 + TOMBSTONE_MS + 1)] {
            let lost = c.claim("e1", who, 1_000, at, None).unwrap();
            assert!(!lost.won, "{who} at {at}");
            assert_eq!(lost.lease.holder, PROCESSED);
            assert!(!c.renew("e1", who, 1_000, at).unwrap().won, "{who} at {at}");
        }
    }

    #[test]
    fn after_a_restart_only_the_holders_renew_recreates_a_pre_boot_lease() {
        let grace = BootGrace {
            until_ms: 10_000,
            watermark_seq: 5,
        };
        let c = EventClaims::open(Some(grace)).unwrap();
        let held = c.claim("old", "b", 1_000, 100, Some(5)).unwrap();
        assert!(!held.won, "a pre-boot event with no row is withheld");
        assert_eq!(held.lease.holder, RECOVERING);
        assert!(
            c.renew("old", "a", 1_000, 200).unwrap().won,
            "its holder renews"
        );
        let lost = c.claim("old", "b", 1_000, 300, Some(5)).unwrap();
        assert_eq!((lost.won, lost.lease.holder.as_str()), (false, "a"));
        assert!(
            c.claim("new", "b", 1_000, 300, Some(6)).unwrap().won,
            "an event captured after the restart is claimable at once"
        );
        assert!(
            c.claim("old2", "b", 1_000, 10_000, Some(1)).unwrap().won,
            "the grace ends"
        );
    }
}
