//! #569: `LlmContextualizer` used to build its client as bare
//! `reqwest::Client::new()` — no timeout. This module's own design
//! promise is that ANY call failure degrades to the structural prefix,
//! never blocks ingest — an unbounded hang silently broke that promise.
//! Same bug class as `gemini.rs`'s embed client, which hung a live pod
//! for real (issue #569's original finding) until kubelet killed it.
//!
//! Gated on the `contextualize-llm` feature — run with
//! `cargo test -p escurel-index --features contextualize-llm`.

#![cfg(feature = "contextualize-llm")]

use std::time::Duration;

use escurel_index::backend::contextualize_llm::LlmContextualizer;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn slow_endpoint_degrades_promptly_instead_of_hanging_forever() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "candidates": [{ "content": { "parts": [{ "text": "a situating sentence" }] } }]
                }))
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    let ctx =
        LlmContextualizer::new(server.uri(), "test-key").with_timeout(Duration::from_millis(200));

    let started = std::time::Instant::now();
    let prefix = ctx
        .context_prefix(Some("Doc Title"), &[], None, "some chunk text")
        .await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "must degrade around the 200ms timeout, not wait for the mock's 5s delay: took {elapsed:?}"
    );
    assert!(
        prefix.is_none(),
        "a timed-out call must degrade to None (caller falls back to the structural prefix), \
         not panic or hang"
    );
}
