//! `init_telemetry` must install a LEVEL FILTER (`RUST_LOG`, default
//! `info`) — previously it installed none, so every process embedding the
//! gateway (a demo, an application test) was flooded with hyper/reqwest
//! TRACE lines and `RUST_LOG` was silently ignored.
//!
//! This test runs in its own process (one test file = one test binary):
//! no subscriber exists before the gateway boots in-process, so
//! `serve()`'s `init_telemetry` installs for real, and the global
//! max-level hint reflects the installed filter. No mocks.

use escurel_test_support::{EscurelProcess, Opts};
use tracing::level_filters::LevelFilter;

#[tokio::test]
async fn default_filter_is_info_not_trace() {
    assert!(
        std::env::var("RUST_LOG").is_err(),
        "test precondition: RUST_LOG unset (cargo test env)"
    );
    let p = EscurelProcess::spawn(Opts::default()).await;

    // The installed subscriber's max-level hint: `info` by default —
    // previously TRACE (no filter at all).
    assert_eq!(
        LevelFilter::current(),
        LevelFilter::INFO,
        "init_telemetry must default the filter to `info`"
    );
    assert!(
        !tracing::enabled!(tracing::Level::TRACE),
        "trace events must be disabled by default"
    );
    assert!(
        tracing::enabled!(tracing::Level::INFO),
        "info events stay enabled"
    );

    p.shutdown().await;
}
