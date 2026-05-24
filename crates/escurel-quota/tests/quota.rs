//! Integration tests for `QuotaManager`. Real `TokenBucket` +
//! real tokio `Semaphore` — no mocks.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use escurel_quota::{Dimension, QuotaConfig, QuotaError, QuotaManager};

fn tiny_config() -> QuotaConfig {
    // Small numbers so tests don't wait for spec defaults.
    QuotaConfig {
        queries_per_minute: 6,    // 1 per 10s
        writes_per_minute: 6_000, // 100/s
        embeds_per_minute: 6_000,
        concurrent_sessions: 2,
    }
}

#[tokio::test]
async fn first_n_queries_succeed_then_exhaust() {
    let qm = QuotaManager::new(tiny_config());
    for _ in 0..6 {
        qm.try_consume("acme", Dimension::Queries).unwrap();
    }
    let err = qm
        .try_consume("acme", Dimension::Queries)
        .expect_err("7th must exhaust");
    match err {
        QuotaError::Exhausted {
            dimension,
            retry_after_ms,
        } => {
            assert_eq!(dimension, Dimension::Queries);
            assert!(retry_after_ms > 0);
            assert!(
                retry_after_ms < 12_000,
                "wait should be ~10s for 6/min, got {retry_after_ms}",
            );
        }
    }
}

#[tokio::test]
async fn buckets_are_per_dimension_independent() {
    let qm = QuotaManager::new(tiny_config());
    for _ in 0..6 {
        qm.try_consume("acme", Dimension::Queries).unwrap();
    }
    // Queries exhausted but writes are still fresh.
    qm.try_consume("acme", Dimension::Writes).unwrap();
    qm.try_consume("acme", Dimension::Embeds).unwrap();
    assert!(qm.try_consume("acme", Dimension::Queries).is_err());
}

#[tokio::test]
async fn buckets_are_per_tenant_isolated() {
    let qm = QuotaManager::new(tiny_config());
    for _ in 0..6 {
        qm.try_consume("acme", Dimension::Queries).unwrap();
    }
    assert!(qm.try_consume("acme", Dimension::Queries).is_err());
    // Globex's first 6 are fresh.
    for _ in 0..6 {
        qm.try_consume("globex", Dimension::Queries).unwrap();
    }
    assert!(qm.try_consume("globex", Dimension::Queries).is_err());
}

#[tokio::test]
async fn per_tenant_override_replaces_defaults() {
    let qm = QuotaManager::new(tiny_config());
    qm.set_for_tenant(
        "premium",
        QuotaConfig {
            queries_per_minute: 60,
            writes_per_minute: 60,
            embeds_per_minute: 60,
            concurrent_sessions: 10,
        },
    );
    for _ in 0..60 {
        qm.try_consume("premium", Dimension::Queries).unwrap();
    }
    assert!(qm.try_consume("premium", Dimension::Queries).is_err());
    // Default-tenant cap stays low.
    for _ in 0..6 {
        qm.try_consume("acme", Dimension::Queries).unwrap();
    }
    assert!(qm.try_consume("acme", Dimension::Queries).is_err());
}

#[tokio::test]
async fn buckets_refill_with_time() {
    let qm = QuotaManager::new(QuotaConfig {
        queries_per_minute: 6_000, // 100/s
        writes_per_minute: 6_000,
        embeds_per_minute: 6_000,
        concurrent_sessions: 2,
    });
    for _ in 0..6_000 {
        qm.try_consume("acme", Dimension::Queries).unwrap();
    }
    assert!(qm.try_consume("acme", Dimension::Queries).is_err());
    thread::sleep(Duration::from_millis(50));
    // After 50ms at 100 tok/sec ≈ 5 tokens.
    qm.try_consume("acme", Dimension::Queries).unwrap();
    qm.try_consume("acme", Dimension::Queries).unwrap();
}

#[tokio::test]
async fn concurrent_sessions_cap_holds() {
    let qm = QuotaManager::new(tiny_config());
    let _g1 = qm.try_acquire_session("acme").expect("first slot");
    let _g2 = qm.try_acquire_session("acme").expect("second slot");
    assert!(
        qm.try_acquire_session("acme").is_none(),
        "third request must fail (cap is 2)",
    );
}

#[tokio::test]
async fn dropping_session_guard_frees_a_slot() {
    let qm = QuotaManager::new(tiny_config());
    let g1 = qm.try_acquire_session("acme").unwrap();
    let _g2 = qm.try_acquire_session("acme").unwrap();
    assert!(qm.try_acquire_session("acme").is_none());
    drop(g1);
    let _g3 = qm.try_acquire_session("acme").expect("slot freed by drop");
}

#[tokio::test]
async fn acquire_session_async_waits_for_a_slot() {
    let qm = Arc::new(QuotaManager::new(tiny_config()));
    let _g1 = qm.try_acquire_session("acme").unwrap();
    let _g2 = qm.try_acquire_session("acme").unwrap();
    let qm2 = Arc::clone(&qm);
    let task = tokio::spawn(async move {
        let _g = qm2.acquire_session("acme").await;
    });
    // Give the task a chance to await.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !task.is_finished(),
        "must still be blocked on the semaphore"
    );
    // Free a slot by dropping g1.
    drop(_g1);
    // Now the awaiting task can proceed.
    tokio::time::timeout(Duration::from_millis(200), task)
        .await
        .expect("task completes")
        .expect("no panic");
}
