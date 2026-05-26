//! `escurel-server` — the deployable single-binary gateway.
//!
//! 12-factor entry point (CLAUDE.md principle 3): read the
//! `ESCUREL_*` config surface from the environment (over an optional
//! TOML base at `$ESCUREL_CONFIG`), build the real backends, bind the
//! HTTP (`8080`) + gRPC (`8081`) listeners, and run until `SIGTERM` /
//! `SIGINT`. JSON structured logs go to stdout via
//! `escurel_obs::init_telemetry` (installed inside `serve`).
//!
//! Exit codes: `0` on clean shutdown; `1` on a fatal config / wiring
//! error before the server is up.

use escurel_server::EscurelConfig;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Telemetry may or may not be installed yet (a config
            // error can happen before `serve` installs the subscriber),
            // so log to stderr unconditionally as well as via tracing.
            eprintln!("escurel-server: fatal: {e}");
            tracing::error!(error = %e, "escurel-server failed to start");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = EscurelConfig::from_env()?;
    // `build` installs telemetry inside `serve`, so the first
    // structured log line is emitted from there. Surface the bound
    // addresses for operator visibility once we're up.
    let booted = config.build().await?;
    let handle = booted.handle;

    tracing::info!(
        http = %handle.local_addr,
        grpc = ?handle.grpc_addr,
        version = %config.version,
        env = %config.env,
        tenant = %config.tenant,
        embedder_loaded = booted.embedder.is_loaded(),
        "escurel-server up"
    );
    // Also print to stdout so a bare `escurel-server` run (or a test
    // spawning the binary) can observe the bound HTTP address without
    // a tracing subscriber configured for the caller.
    println!("escurel-server listening http={}", handle.local_addr);

    wait_for_shutdown().await;

    tracing::info!("escurel-server received shutdown signal; draining");
    handle.shutdown().await;
    Ok(())
}

/// Block until SIGTERM (Nomad's graceful-stop signal) or SIGINT
/// (Ctrl-C in a dev shell). On non-unix targets, only Ctrl-C.
#[cfg(unix)]
async fn wait_for_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
