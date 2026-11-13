//! `escurel-chat-bridge` — answers people on a messaging platform from
//! escurel (#824). A platform message becomes a page-chat turn plus a `chat`
//! event, captured as the asker; the runner answers it in chat mode; the
//! bridge sends that one answer back.
//!
//! A separate process, so the gateway stays free of platform automation.
//! [`ChatChannel`] is the platform seam; Microsoft Teams
//! ([`teams::TeamsChannel`]) is the implementation today. Operator guide:
//! `docs/deploy/chat-bridge.md`.

pub mod bridge;
pub mod channel;
pub mod config;
pub mod fetch;
mod http;
pub mod store;
pub mod teams;

pub use bridge::{Bridge, BridgeError};
pub use channel::{Attachment, ChannelError, ChatChannel, InboundError, InboundTurn, UserRef};
pub use config::{BridgeConfig, ConfigError};
pub use store::Store;

/// Why the bridge could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error("signing key: {0}")]
    Signer(String),
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    #[error("listen: {0}")]
    Io(#[from] std::io::Error),
}

/// Open the state, resume unfinished turns, serve until SIGTERM / Ctrl-C.
///
/// # Errors
/// The state file, the signing key or the listener cannot be set up.
pub async fn run(cfg: BridgeConfig) -> Result<(), StartError> {
    let store = Store::open(&cfg.state_path)?;
    let g = &cfg.gateway;
    let signer = escurel_auth::Signer::build(
        g.issuer.clone(),
        g.audience.clone(),
        g.tenant.clone(),
        Some(g.kid.clone()),
        &g.signing_key,
    )
    .map_err(|e| StartError::Signer(e.to_string()))?;
    let channel = teams::TeamsChannel::new(cfg.teams.clone());
    let listen = cfg.listen.clone();
    let version = cfg.version.clone();
    let bridge = Bridge::new(cfg, channel, store, signer);
    let resumed = bridge.resume()?;
    bridge.start_sweep();
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(listen = %listen, resumed, "escurel-chat-bridge listening");
    axum::serve(listener, http::router(bridge, version))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}
