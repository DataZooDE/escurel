//! Thin entry point: read the `EXPLORE_BFF_*` env surface, build the
//! router, and serve. All behaviour lives in the library so tests can
//! construct the app in-process. See [`escurel_explore_bff`].

use escurel_explore_bff::{Config, app, listen_from_env, serve};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Structured JSON logs to stdout (12-factor / substrate contract).
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::from_env();
    let listen = listen_from_env();
    tracing::info!(
        backend = %cfg.backend,
        issuer = %cfg.issuer,
        audience = %cfg.audience,
        tenant = %cfg.tenant,
        kid = %cfg.kid,
        bundle_dir = %cfg.bundle_dir,
        "escurel-explore-bff starting"
    );
    serve(&listen, app(cfg)).await
}
