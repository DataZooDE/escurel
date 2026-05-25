//! `echo-app` binary entrypoint.
//!
//! Reads its escurel coordinates from the environment and runs the
//! axum backend until SIGTERM / SIGINT. Production-shaped; the
//! tests construct [`echo_app::Opts`] directly and use the
//! `escurel-test-support` façade for the upstream.

use std::net::SocketAddr;

use anyhow::Context as _;
use tokio::signal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let opts = echo_app::env_opts().context("read ESCUREL_* env vars")?;
    let backend = echo_app::spawn(opts)
        .await
        .context("spawn echo-app backend")?;

    // The library binds 127.0.0.1:0 by default. For the binary, an
    // operator usually wants to publish on a known port. We
    // re-bind on the configured PORT (or 8080) by piping the
    // request through axum directly — but the library shape is
    // designed for the test harness. Keep this binary minimal:
    // log the random URL the library bound and serve until
    // shutdown.
    //
    // Production deployments wire PORT into the listener at the
    // library layer; that's a follow-up if echo-app ever ships
    // outside the example role.
    let advertised: SocketAddr = backend
        .base_url()
        .trim_start_matches("http://")
        .parse()
        .context("parse backend base_url")?;
    tracing::info!(addr = %advertised, "echo-app backend ready");

    shutdown_signal().await;
    backend.shutdown().await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
