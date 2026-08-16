//! #569: `JwksCache` used to build its client as bare
//! `reqwest::Client::new()` — no timeout, so a hung/slow JWKS endpoint
//! would block every request behind an expired TTL forever. Found
//! alongside the same gap in `GeminiEmbedder`, which hung a whole pod's
//! request handling for real against `lab` until kubelet force-restarted
//! it. This is the same failure mode on the AUTHENTICATED-REQUEST path,
//! a wider blast radius than a single ingest call.

use std::time::Duration;

use escurel_auth::JwksCache;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn slow_jwks_endpoint_times_out_instead_of_hanging_forever() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "keys": [] }))
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    let cache = JwksCache::new(server.uri(), Duration::from_secs(300))
        .with_timeout(Duration::from_millis(200));

    let started = std::time::Instant::now();
    cache
        .refresh()
        .await
        .expect_err("a JWKS endpoint slower than the timeout must error, not hang");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "must fail around the 200ms timeout, not wait for the mock's 5s delay: took {elapsed:?}"
    );
}
