//! Integration tests for `HashEmbedder`.

use escurel_embed::{Embedder, HashEmbedder};

#[tokio::test]
async fn different_texts_produce_different_vectors() {
    let e = HashEmbedder::default();
    let out = e.embed(&["one", "two", "three"]).await.unwrap();
    assert_eq!(out.len(), 3);
    assert_ne!(out[0], out[1]);
    assert_ne!(out[1], out[2]);
    assert_ne!(out[0], out[2]);
}

#[tokio::test]
async fn same_text_produces_same_vector_across_calls() {
    let e = HashEmbedder::default();
    let a = e.embed(&["hello"]).await.unwrap();
    let b = e.embed(&["hello"]).await.unwrap();
    assert_eq!(a[0], b[0], "hashing must be deterministic");
}

#[tokio::test]
async fn output_vectors_have_configured_dim() {
    let e = HashEmbedder::new(384);
    let out = e.embed(&["x"]).await.unwrap();
    assert_eq!(out[0].len(), 384);
}

#[tokio::test]
async fn vectors_are_l2_normalised() {
    let e = HashEmbedder::default();
    let out = e.embed(&["whatever"]).await.unwrap();
    let norm: f32 = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-5,
        "L2 norm should be 1.0, got: {norm}",
    );
}
