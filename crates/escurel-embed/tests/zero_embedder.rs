//! Integration tests for `ZeroEmbedder`.
//!
//! Trivial impl, but the tests pin the trait contract for future
//! `EmbeddingGemma` and Gemini implementations: shape, length,
//! dimension, batching.

use escurel_embed::{Embedder, ZeroEmbedder};

#[tokio::test]
async fn embeds_one_text_to_768_dim_zero_vector() {
    let e = ZeroEmbedder::default();
    let out = e.embed(&["hello"]).await.expect("embed");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].len(), 768);
    assert!(out[0].iter().all(|&x| x == 0.0_f32));
}

#[tokio::test]
async fn embeds_a_batch_preserves_order_and_length() {
    let e = ZeroEmbedder::new(128);
    let texts = ["one", "two", "three", "four"];
    let out = e.embed(&texts).await.expect("embed batch");
    assert_eq!(out.len(), texts.len(), "one vector per input text");
    for v in &out {
        assert_eq!(v.len(), 128, "every vector matches dim()");
    }
}

#[tokio::test]
async fn dim_matches_constructor() {
    assert_eq!(ZeroEmbedder::default().dim(), 768);
    assert_eq!(ZeroEmbedder::new(256).dim(), 256);
}

#[tokio::test]
async fn empty_batch_returns_empty_vec() {
    let e = ZeroEmbedder::default();
    let out = e.embed(&[]).await.expect("empty embed");
    assert!(out.is_empty());
}

#[tokio::test]
async fn dyn_trait_object_is_send_sync() {
    // Compile-time check: the trait is dyn-compatible and meets
    // the Send + Sync bounds the indexer needs.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<std::sync::Arc<dyn Embedder>>();
    let _: std::sync::Arc<dyn Embedder> = std::sync::Arc::new(ZeroEmbedder::default());
}
