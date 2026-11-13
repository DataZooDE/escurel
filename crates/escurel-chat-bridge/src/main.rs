//! Entry point: read `ESCUREL_BRIDGE_*`, install JSON logs, serve.

use escurel_chat_bridge::{BridgeConfig, run};
use escurel_obs::{TelemetryConfig, init_telemetry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = BridgeConfig::from_env()?;
    let _telemetry = init_telemetry(TelemetryConfig {
        app: "escurel-chat-bridge".to_owned(),
        env: cfg.env.clone(),
        version: cfg.version.clone(),
        otlp_endpoint: std::env::var("ESCUREL_OTLP_ENDPOINT").ok(),
        json_logs: true,
    })?;
    run(cfg).await?;
    Ok(())
}
