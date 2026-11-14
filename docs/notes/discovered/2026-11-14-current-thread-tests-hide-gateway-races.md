# A `#[tokio::test]` hides races in the in-process gateway

**Symptom.** A test fires N concurrent requests at an `EscurelProcess`
gateway to prove a check-then-act is atomic (the schedule webhook's budget:
count today's triggers, then capture one). It passes — and keeps passing
with the lock that makes it atomic deleted.

**Why.** `#[tokio::test]` is a current-thread runtime, and
`EscurelProcess` serves the gateway on that same runtime. The indexer's
DuckDB calls are synchronous behind an uncontended `tokio::sync::Mutex`,
which does not yield, so each handler runs from its first indexer call to
its capture without another handler getting a turn. The requests are
concurrent on the wire and serial in the gateway — unlike production, where
the multi-threaded runtime runs them in parallel (16 of 16 racing
deliveries were admitted against a budget of 1 once the test ran
multi-threaded).

**Fix.** A test that exists to catch a race runs
`#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`. Then plant
the defect (delete the lock) and watch it go red before trusting it green.

**Recognise it.** Any concurrency test against an in-process gateway on the
default test runtime: `crates/escurel-runner/tests/suite/schedule_webhook.rs`
`concurrent_deliveries_admit_exactly_the_budget` is the worked example.
