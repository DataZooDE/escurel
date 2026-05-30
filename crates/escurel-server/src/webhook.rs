//! Outbound capture webhook (M7 event-sourcing surface).
//!
//! Opt-in via `ESCUREL_WEBHOOK_URL`. When configured, the gateway fires
//! a **fire-and-forget** HTTP POST carrying the captured event's JSON
//! each time `capture_event` lands a new inbox item — the notification
//! an external processing agent subscribes to (it may also poll
//! `list_inbox`, so a missed POST self-heals). Delivery never blocks or
//! fails the capture: a down sink is logged and dropped.

use std::time::Duration;

use serde_json::Value;

/// A configured outbound webhook target. Cheap to clone (the inner
/// `reqwest::Client` is an `Arc`); held on `AppState`.
#[derive(Clone)]
pub(crate) struct Webhook {
    url: String,
    client: reqwest::Client,
}

impl Webhook {
    /// Build a webhook for `url`. Falls back to a default client if the
    /// builder fails (it never does with these options).
    pub(crate) fn new(url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self { url, client }
    }

    /// POST `event` as JSON, fire-and-forget. Spawns a detached task so
    /// the capture response path is never blocked by webhook latency;
    /// transport errors are logged, not propagated.
    pub(crate) fn notify(&self, event: Value) {
        let url = self.url.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            match client.post(&url).json(&event).send().await {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => tracing::warn!(
                    target: "escurel",
                    url = %url,
                    status = %resp.status(),
                    "capture webhook: non-success status"
                ),
                Err(e) => tracing::warn!(
                    target: "escurel",
                    url = %url,
                    error = %e,
                    "capture webhook: POST failed"
                ),
            }
        });
    }
}
