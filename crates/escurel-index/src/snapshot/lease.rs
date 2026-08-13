//! The single-writer lease against a shared DuckLake catalog (#371).
//!
//! Two replicas both running `ESCUREL_ROLE=writer` against one catalog
//! lose acknowledged writes: each publishes its own snapshot of the
//! whole lake and prunes parquet files the other just committed, so the
//! visible page count *falls over time* (measured 17/40 survivors in
//! the issue's reproducer) while every write was acknowledged `ok`.
//! Nothing in the config surface made `replicaCount: 2` look unsafe —
//! this lease makes the misconfiguration a loud boot failure instead of
//! silent data loss.
//!
//! Mechanism: a Postgres **advisory lock** (`pg_try_advisory_lock`)
//! taken on a dedicated `tokio-postgres` session against the catalog
//! database and held for the writer's lifetime. Advisory locks are
//! per-database, so prod/nonprod catalogs sharing one Postgres server
//! stay independent, and they release automatically when the holding
//! session ends — a crashed writer never wedges its successor, and the
//! Kamal STOP-FIRST redeploy (old writer stops, then the new one
//! starts) hands over cleanly.
//!
//! The lease is a boot-time guard, not a distributed-consensus fence:
//! it stops the known-fatal two-writer topology at startup. Readers
//! never take it.

use tokio::task::JoinHandle;

/// The advisory-lock key every escurel writer contends on, within one
/// catalog database. Arbitrary but stable — changing it would let a new
/// build boot alongside an old writer. (Spells "escurel" on a phone
/// keypad, suffixed `1` for the writer-lease lock class.)
const WRITER_LEASE_KEY: i64 = 372_873_501;

/// A held single-writer lease. Dropping it ends the holding session,
/// which releases the advisory lock on the catalog.
pub struct WriterLease {
    /// Owns the Postgres session the lock lives on.
    client: tokio_postgres::Client,
    /// Drives that session's I/O; aborted (not awaited) on drop.
    driver: JoinHandle<()>,
}

impl WriterLease {
    /// Try to take the single-writer lease on the catalog at `dsn`
    /// (keyword-value or URI form, as `ESCUREL_DUCKLAKE_CATALOG_DSN`).
    ///
    /// - `Ok(Some(lease))` — this process is the writer; keep the value
    ///   alive for the process lifetime.
    /// - `Ok(None)` — another live session already holds the lease:
    ///   refuse to boot as writer.
    /// - `Err(_)` — the catalog was unreachable over this client (which
    ///   speaks plain TCP, no TLS): the caller decides whether that is
    ///   fatal or the guard is explicitly disabled.
    pub async fn acquire(dsn: &str) -> Result<Option<Self>, tokio_postgres::Error> {
        let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls).await?;
        let driver = tokio::spawn(async move {
            // A closed connection ends the session and thereby the
            // lock; the acquire-side `Err` path already covers boot,
            // and mid-run loss is indistinguishable from process death
            // to the successor, so nothing to do here.
            let _ = connection.await;
        });
        let row = client
            .query_one("SELECT pg_try_advisory_lock($1)", &[&WRITER_LEASE_KEY])
            .await?;
        if row.get::<_, bool>(0) {
            Ok(Some(Self { client, driver }))
        } else {
            driver.abort();
            Ok(None)
        }
    }
}

impl Drop for WriterLease {
    fn drop(&mut self) {
        // Dropping `client` closes the session, which releases the
        // advisory lock server-side; aborting the driver just stops the
        // now-pointless I/O task.
        let _ = &self.client;
        self.driver.abort();
    }
}
