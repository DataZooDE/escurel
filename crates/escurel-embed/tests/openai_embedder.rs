//! Integration tests for `OpenAiEmbedder` against a real local HTTP server
//! (wiremock) speaking the OpenAI `/embeddings` shape. Gated on the `openai`
//! feature. No real provider calls.

#![cfg(feature = "openai")]

use std::time::Duration;

use escurel_embed::{EmbedError, Embedder, OpenAiEmbedder};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "sk-test-not-a-real-credential";

fn vector(dim: usize, seed: f32) -> Vec<f32> {
    (0..dim).map(|i| (i as f32).mul_add(0.001, seed)).collect()
}

/// Echo one vector per input, OUT OF ORDER, so the test proves the embedder
/// reorders by `index`.
struct Echo(usize);
impl wiremock::Respond for Echo {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).expect("json");
        let n = body["input"].as_array().map_or(0, Vec::len);
        let data: Vec<Value> = (0..n)
            .rev()
            .map(|i| json!({ "object": "embedding", "index": i, "embedding": vector(self.0, i as f32) }))
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({
            "object": "list", "model": body["model"], "data": data,
            "usage": { "prompt_tokens": 3, "total_tokens": 3 },
        }))
    }
}

fn embedder(server: &MockServer, dim: usize) -> OpenAiEmbedder {
    OpenAiEmbedder::new(
        "alibaba",
        &format!("{}/compatible-mode/v1", server.uri()),
        KEY,
        "text-embedding-v4",
        dim,
    )
    .expect("configured")
    .with_retry_backoff(Duration::from_millis(5))
}

#[tokio::test]
async fn embeds_in_batches_with_bearer_auth_dimensions_and_index_order() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/compatible-mode/v1/embeddings"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(Echo(8))
        .expect(3)
        .mount(&server)
        .await;
    let e = embedder(&server, 8).with_max_batch(10);
    let texts: Vec<String> = (0..25).map(|i| format!("text {i}")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let out = e.embed(&refs).await.expect("embeds");
    assert_eq!(out.len(), 25);
    // Order restored by `index` within each batch of 10.
    assert_eq!(out[0], vector(8, 0.0));
    assert_eq!(out[9], vector(8, 9.0));
    assert_eq!(out[10], vector(8, 0.0));
    let reqs = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&reqs[0].body).expect("json");
    assert_eq!(body["model"], "text-embedding-v4");
    assert_eq!(body["dimensions"], 8);
    assert_eq!(body["encoding_format"], "float");
    assert_eq!(body["input"].as_array().map(Vec::len), Some(10));
    assert!(
        reqs.iter().all(|r| !r.url.as_str().contains(KEY)),
        "the key travels in a header, never the URL"
    );
}

#[tokio::test]
async fn the_model_identity_names_provider_and_model() {
    let server = MockServer::start().await;
    let e = embedder(&server, 8);
    assert_eq!(e.model_id(), "alibaba:text-embedding-v4");
    assert!(!format!("{e:?}").contains(KEY), "Debug redacts the key");
}

#[tokio::test]
async fn rate_limits_are_waited_out_and_4xx_is_not() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string("Throttling"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(Echo(4))
        .mount(&server)
        .await;
    let out = embedder(&server, 4)
        .embed(&["a"])
        .await
        .expect("after retries");
    assert_eq!(out.len(), 1);
    assert_eq!(server.received_requests().await.expect("reqs").len(), 3);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(format!("Incorrect API key {KEY}")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = embedder(&server, 4).embed(&["a"]).await.expect_err("401");
    assert!(err.to_string().contains("401"), "{err}");
    assert!(!err.to_string().contains(KEY), "the key is redacted: {err}");
}

#[tokio::test]
async fn a_wrong_dimension_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Echo(1024))
        .mount(&server)
        .await;
    let err = embedder(&server, 768).embed(&["a"]).await.expect_err("dim");
    assert!(
        matches!(
            err,
            EmbedError::DimensionMismatch {
                expected: 768,
                got: 1024
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn duplicate_or_missing_indices_are_rejected() {
    for data in [
        json!([{ "index": 0, "embedding": vector(4, 0.0) }, { "index": 0, "embedding": vector(4, 1.0) }]),
        json!([{ "index": 0, "embedding": vector(4, 0.0) }, { "index": 2, "embedding": vector(4, 1.0) }]),
        json!([{ "embedding": vector(4, 0.0) }, { "embedding": vector(4, 1.0) }]),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": data })))
            .mount(&server)
            .await;
        let err = embedder(&server, 4).embed(&["a", "b"]).await;
        assert!(
            err.is_err(),
            "{data}: vectors must map to inputs one to one"
        );
    }
}

#[test]
fn misconfiguration_is_refused() {
    // `localhost` is a name and may resolve off the host: plain http only to
    // literal loopback addresses.
    assert!(OpenAiEmbedder::new("x", "http://localhost:9/v1", KEY, "m", 8).is_err());
    assert!(OpenAiEmbedder::new("x", "http://127.0.0.1:9/v1", KEY, "m", 8).is_ok());
    let err =
        OpenAiEmbedder::new("x", "https://u:SECRET@embed.example/v1", KEY, "m", 8).unwrap_err();
    assert!(!err.to_string().contains("SECRET"), "{err}");
    assert!(OpenAiEmbedder::new("x", "http://embed.example/v1", KEY, "m", 8).is_err());
    assert!(OpenAiEmbedder::new("x", "https://embed.example/v1", " ", "m", 8).is_err());
    assert!(OpenAiEmbedder::new("x", "https://embed.example/v1", KEY, "", 8).is_err());
    assert!(OpenAiEmbedder::new("x", "https://embed.example/v1?key=1", KEY, "m", 8).is_err());
    assert!(OpenAiEmbedder::new("x", "https://embed.example/v1", KEY, "m", 8).is_ok());
}
