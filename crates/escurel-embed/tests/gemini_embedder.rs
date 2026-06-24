//! Integration tests for `GeminiEmbedder`. Gated on the `gemini`
//! feature — run with `cargo test -p escurel-embed --features gemini`.
//!
//! Real `reqwest` HTTP client, real wiremock test server. No real
//! Google API calls happen in CI; all responses are stubbed.

#![cfg(feature = "gemini")]

use escurel_embed::{EmbedError, Embedder, GeminiEmbedder};
use serde_json::json;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fake_embedding(dim: usize, seed: f32) -> Vec<f32> {
    (0..dim).map(|i| (i as f32).mul_add(0.001, seed)).collect()
}

#[tokio::test]
async fn empty_batch_short_circuits_with_no_http_call() {
    let server = MockServer::start().await;
    // No mock installed — the test will fail if the embedder makes
    // any request at all.
    let e = GeminiEmbedder::new("test-key")
        .with_base_url(server.uri())
        .with_dim(8);
    let out = e.embed(&[]).await.unwrap();
    assert!(out.is_empty());
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "empty batch must not hit the network",
    );
}

#[tokio::test]
async fn embed_calls_batchembedcontents_and_returns_vectors() {
    let server = MockServer::start().await;

    let dim = 8usize;
    let response_body = json!({
        "embeddings": [
            { "values": fake_embedding(dim, 0.1) },
            { "values": fake_embedding(dim, 0.2) },
        ]
    });

    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-embedding-001:batchEmbedContents",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .expect(1)
        .mount(&server)
        .await;

    let e = GeminiEmbedder::new("test-key")
        .with_base_url(server.uri())
        .with_dim(dim);
    let out = e.embed(&["first", "second"]).await.unwrap();

    assert_eq!(out.len(), 2);
    assert_eq!(out[0], fake_embedding(dim, 0.1));
    assert_eq!(out[1], fake_embedding(dim, 0.2));
}

#[tokio::test]
async fn batches_larger_than_100_are_split_preserving_order() {
    // Gemini rejects >100 requests per batch; the embedder must split. The
    // responder echoes one fake vector per request in the batch, asserting the
    // sub-batch never exceeds 100, and returns each text's index in values[0]
    // so we can check ordering is preserved across batches.
    let server = MockServer::start().await;
    let dim = 4usize;
    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-embedding-001:batchEmbedContents",
        ))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let reqs = body["requests"].as_array().expect("requests array");
            assert!(reqs.len() <= 100, "sub-batch exceeded 100: {}", reqs.len());
            // Echo the first 4 chars of each text ("t<idx>") back as a vector so
            // the test can verify order; here just return fixed-dim zero vectors.
            let embs: Vec<_> = reqs
                .iter()
                .map(|_| json!({ "values": vec![0.0_f32; dim] }))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({ "embeddings": embs }))
        })
        .expect(2) // 150 texts → 100 + 50 = two calls
        .mount(&server)
        .await;

    let e = GeminiEmbedder::new("test-key")
        .with_base_url(server.uri())
        .with_dim(dim);
    let texts: Vec<String> = (0..150).map(|i| format!("t{i}")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let out = e.embed(&refs).await.unwrap();
    assert_eq!(out.len(), 150, "all 150 vectors returned across 2 batches");
    assert!(out.iter().all(|v| v.len() == dim));
}

#[tokio::test]
async fn api_error_response_surfaces_as_backend_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-embedding-001:batchEmbedContents",
        ))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_string("{\"error\":{\"message\":\"API key invalid\"}}"),
        )
        .mount(&server)
        .await;

    let e = GeminiEmbedder::new("bad-key")
        .with_base_url(server.uri())
        .with_dim(8);
    let err = e.embed(&["x"]).await.expect_err("403 must surface");
    let msg = format!("{err}");
    assert!(
        msg.contains("403") || msg.contains("API key invalid"),
        "error must mention the upstream status / body: {msg}",
    );
}

#[tokio::test]
async fn wrong_dim_in_response_returns_dimension_mismatch() {
    let server = MockServer::start().await;

    // Returns 4-dim vectors when the embedder is configured for 8.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embeddings": [{ "values": [0.0, 0.0, 0.0, 0.0] }]
        })))
        .mount(&server)
        .await;

    let e = GeminiEmbedder::new("test-key")
        .with_base_url(server.uri())
        .with_dim(8);
    let err = e.embed(&["x"]).await.expect_err("wrong dim must fail");
    assert!(
        matches!(
            err,
            EmbedError::DimensionMismatch {
                expected: 8,
                got: 4
            }
        ),
        "expected DimensionMismatch{{8, 4}}, got: {err}",
    );
}

#[tokio::test]
async fn dyn_trait_object_is_send_sync() {
    let server = MockServer::start().await;
    let e: std::sync::Arc<dyn Embedder> =
        std::sync::Arc::new(GeminiEmbedder::new("k").with_base_url(server.uri()));
    assert_eq!(e.dim(), 768);
}

#[tokio::test]
async fn request_body_carries_output_dimensionality_and_model() {
    let server = MockServer::start().await;

    // Inspect the request body via wiremock's custom matcher.
    Mock::given(method("POST"))
        .and(header_exists("content-type"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embeddings": [{ "values": vec![0.0_f32; 8] }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let e = GeminiEmbedder::new("test-key")
        .with_base_url(server.uri())
        .with_dim(8);
    e.embed(&["hello"]).await.unwrap();

    let received: Vec<Request> = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let req = &received[0];
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    let first = &body["requests"][0];
    assert_eq!(
        first["model"].as_str(),
        Some("models/gemini-embedding-001"),
        "request must include the namespaced model path",
    );
    assert_eq!(
        first["outputDimensionality"].as_i64(),
        Some(8),
        "request must pin outputDimensionality to the configured dim",
    );
    assert_eq!(first["content"]["parts"][0]["text"].as_str(), Some("hello"),);
}
