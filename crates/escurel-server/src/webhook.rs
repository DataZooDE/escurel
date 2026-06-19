//! Outbound capture webhook (M7 event-sourcing surface).
//!
//! Opt-in via `ESCUREL_WEBHOOK_URL`. When configured, the gateway fires
//! a **fire-and-forget** HTTP POST carrying the captured event's JSON
//! each time `capture_event` lands a new inbox item — the notification
//! an external processing agent (the `escurel-runner`) subscribes to (it
//! may also poll `list_inbox`, so a missed POST self-heals). Delivery
//! never blocks or fails the capture: a down sink is logged and dropped.
//!
//! ## Authenticity + tenant identity (#147)
//!
//! The delivered payload always carries the gateway's authoritative
//! `tenant_id` (the gateway is single-tenant per indexer, so
//! `indexer.tenant()` is the source of truth) so the receiver can tell
//! which tenant the event belongs to without a side channel.
//!
//! When `ESCUREL_WEBHOOK_SECRET` is set, the body bytes are signed with
//! HMAC-SHA256 and the signature is sent as
//! `X-Escurel-Webhook-Signature: sha256=<hex>`. The body is serialized
//! **once** to bytes, those exact bytes are signed, and those exact bytes
//! are POSTed — so the signature the receiver recomputes over the raw
//! request body always matches (serialize-twice could differ).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Header carrying the HMAC-SHA256 signature of the POST body, in the
/// form `sha256=<lowercase-hex>`.
const SIGNATURE_HEADER: &str = "X-Escurel-Webhook-Signature";

/// How many recent delivery records the in-memory log retains. A live
/// observability window, not durable history — the buffer is per-process
/// and resets on restart.
const DELIVERY_LOG_CAP: usize = 200;

/// One outbound-webhook delivery attempt and its outcome — the record the
/// `admin_webhook_deliveries` tool surfaces so operators can see whether
/// captures are reaching the agent runner.
#[derive(Debug, Clone)]
pub(crate) struct DeliveryRecord {
    /// `event_id` of the captured event that triggered the POST.
    pub event_id: String,
    /// Unix-millis timestamp of the delivery outcome.
    pub at_ms: u64,
    /// `true` when the sink returned a 2xx.
    pub ok: bool,
    /// HTTP status code, when a response was received (`None` on a
    /// transport error).
    pub http_status: Option<u16>,
    /// Transport/error detail, when the POST failed.
    pub error: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A configured outbound webhook target. Cheap to clone (the inner
/// `reqwest::Client` is an `Arc`); held on `AppState`.
#[derive(Clone)]
pub(crate) struct Webhook {
    url: String,
    /// Optional shared secret. When `Some`, the body is HMAC-SHA256
    /// signed and the signature is sent in [`SIGNATURE_HEADER`].
    secret: Option<String>,
    client: reqwest::Client,
    /// Bounded in-memory log of recent delivery outcomes (newest last).
    /// `Arc<Mutex<…>>` so the detached `notify` task can record into the
    /// same buffer the `admin_webhook_deliveries` reader sees. Cheap to
    /// clone with the rest of `Webhook`.
    deliveries: Arc<Mutex<VecDeque<DeliveryRecord>>>,
}

impl Webhook {
    /// Build a webhook for `url` with an optional signing `secret`.
    /// Falls back to a default client if the builder fails (it never
    /// does with these options).
    pub(crate) fn new(url: String, secret: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            url,
            secret,
            client,
            deliveries: Arc::new(Mutex::new(VecDeque::with_capacity(DELIVERY_LOG_CAP))),
        }
    }

    /// Recent delivery outcomes, newest first, capped at `limit` (and at
    /// [`DELIVERY_LOG_CAP`] regardless). For the operator deliveries view.
    pub(crate) fn recent(&self, limit: usize) -> Vec<DeliveryRecord> {
        let log = self.deliveries.lock().expect("delivery log mutex");
        log.iter().rev().take(limit).cloned().collect()
    }

    /// POST `event` as JSON, fire-and-forget. The authoritative
    /// `tenant_id` is injected into the payload (overwriting any inbound
    /// value) so the receiver always knows the tenant. When a secret is
    /// configured the body is HMAC-SHA256-signed.
    ///
    /// Spawns a detached task so the capture response path is never
    /// blocked by webhook latency; transport errors are logged, not
    /// propagated.
    pub(crate) fn notify(&self, mut event: Value, tenant_id: &str) {
        // Inject the authoritative tenant identity into the payload.
        if let Value::Object(map) = &mut event {
            map.insert("tenant_id".to_owned(), Value::String(tenant_id.to_owned()));
        }

        // Serialize ONCE to the exact bytes we sign and send, so the
        // signature matches what the receiver verifies over the raw body.
        let body = match serde_json::to_vec(&event) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    target: "escurel",
                    error = %e,
                    "capture webhook: failed to serialize payload"
                );
                return;
            }
        };
        let signature = self.secret.as_deref().map(|secret| sign(secret, &body));

        // The event_id the delivery record is keyed by (best-effort).
        let event_id = event
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        let url = self.url.clone();
        let client = self.client.clone();
        let deliveries = Arc::clone(&self.deliveries);
        tokio::spawn(async move {
            let (ok, http_status, error) = match req_send(&client, &url, body, signature).await {
                Ok(status) if status < 400 => (true, Some(status), None),
                Ok(status) => {
                    tracing::warn!(
                        target: "escurel", url = %url, status,
                        "capture webhook: non-success status"
                    );
                    (false, Some(status), None)
                }
                Err(e) => {
                    tracing::warn!(
                        target: "escurel", url = %url, error = %e,
                        "capture webhook: POST failed"
                    );
                    (false, None, Some(e))
                }
            };
            let mut log = deliveries.lock().expect("delivery log mutex");
            if log.len() == DELIVERY_LOG_CAP {
                log.pop_front();
            }
            log.push_back(DeliveryRecord {
                event_id,
                at_ms: now_ms(),
                ok,
                http_status,
                error,
            });
        });
    }
}

/// Send the POST, returning the HTTP status code on response or the
/// transport error string. Factored out so `notify`'s task reads cleanly.
async fn req_send(
    client: &reqwest::Client,
    url: &str,
    body: Vec<u8>,
    signature: Option<String>,
) -> Result<u16, String> {
    let mut req = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    if let Some(sig) = signature {
        req = req.header(SIGNATURE_HEADER, sig);
    }
    req.send()
        .await
        .map(|resp| resp.status().as_u16())
        .map_err(|e| e.to_string())
}

/// Compute `sha256=<lowercase-hex>` HMAC-SHA256 of `body` under `secret`.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts a key of any size");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity("sha256=".len() + bytes.len() * 2);
    hex.push_str("sha256=");
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sign_is_stable_and_sensitive() {
        let s = sign("k", b"hello");
        assert!(s.starts_with("sha256="));
        // 32-byte digest → 64 hex chars after the prefix.
        assert_eq!(s.len(), "sha256=".len() + 64);
        assert_eq!(s, sign("k", b"hello"), "deterministic");
        assert_ne!(s, sign("k2", b"hello"), "key-sensitive");
        assert_ne!(s, sign("k", b"hello!"), "body-sensitive");
    }

    #[test]
    fn known_answer_matches_published_vector() {
        // HMAC-SHA256("key", "The quick brown fox jumps over the lazy dog")
        let s = sign("key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            s,
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn tenant_id_injection_overwrites_any_inbound_value() {
        let mut event = json!({"event_id": "e1", "tenant_id": "stale"});
        if let Value::Object(map) = &mut event {
            map.insert("tenant_id".to_owned(), Value::String("carl".to_owned()));
        }
        assert_eq!(event["tenant_id"], json!("carl"));
    }
}
