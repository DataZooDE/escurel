//! `JwksCache` refreshes on an unknown kid, but at most once per interval and
//! never twice at once (crew security F3). The bridge's `/api/messages` is
//! public and looks a kid up before any signature check, so without this every
//! request with a random kid was an outbound fetch to the key's issuer
//! (Microsoft, for Teams) — a cheap way to get the bot throttled upstream.
//! Key rotation must still work: a new kid after the interval is fetched.

use std::time::Duration;

use escurel_auth::JwksCache;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn jwk(kid: &str) -> Value {
    use base64::Engine as _;
    let private = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("rsa keygen");
    let public = RsaPublicKey::from(&private);
    let b64 = |b: Vec<u8>| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    json!({ "kid": kid, "kty": "RSA", "alg": "RS256",
            "n": b64(public.n().to_bytes_be()), "e": b64(public.e().to_bytes_be()) })
}

async fn serve(server: &MockServer, keys: &[Value]) {
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "keys": keys }))
                // Long enough that the concurrent lookups below overlap.
                .set_delay(Duration::from_millis(200)),
        )
        .mount(server)
        .await;
}

async fn fetches(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

#[tokio::test]
async fn concurrent_unknown_kids_cost_one_fetch_and_a_rotated_kid_is_found_after_the_interval() {
    let server = MockServer::start().await;
    let a = jwk("a");
    serve(&server, std::slice::from_ref(&a)).await;
    let cache = JwksCache::new(server.uri(), Duration::from_secs(300))
        .with_min_refresh_interval(Duration::from_millis(800));

    // N concurrent lookups of kids nobody publishes: one fetch between them.
    let lookups: Vec<_> = (0..20)
        .map(|i| {
            let cache = cache.clone();
            tokio::spawn(async move { cache.key_for_kid(&format!("random-{i}")).await })
        })
        .collect();
    for l in lookups {
        assert!(l.await.unwrap().is_err(), "an unpublished kid is unknown");
    }
    assert_eq!(
        fetches(&server).await,
        1,
        "concurrent misses share one fetch"
    );

    // A miss right after a refresh does not fetch again; a known kid is served.
    assert!(cache.key_for_kid("random-again").await.is_err());
    cache.key_for_kid("a").await.expect("a published kid");
    assert_eq!(fetches(&server).await, 1, "a recent refresh answers misses");

    // Rotation: the issuer publishes `b`. Within the interval it is not
    // fetched yet; after it, the next lookup fetches and finds it.
    serve(&server, &[a, jwk("b")]).await;
    assert!(
        cache.key_for_kid("b").await.is_err(),
        "still inside the interval"
    );
    assert_eq!(fetches(&server).await, 0, "no fetch inside the interval");
    tokio::time::sleep(Duration::from_millis(900)).await;
    cache
        .key_for_kid("b")
        .await
        .expect("a rotated-in kid after the interval");
    assert_eq!(fetches(&server).await, 1, "one fetch for the rotation");
}

/// A failing endpoint is not hammered either: concurrent misses while the
/// fetch fails share that one failed attempt.
#[tokio::test]
async fn concurrent_misses_share_one_failed_fetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_delay(Duration::from_millis(200)))
        .mount(&server)
        .await;
    let cache = JwksCache::new(server.uri(), Duration::from_secs(300));
    let lookups: Vec<_> = (0..10)
        .map(|i| {
            let cache = cache.clone();
            tokio::spawn(async move { cache.key_for_kid(&format!("k-{i}")).await })
        })
        .collect();
    for l in lookups {
        assert!(l.await.unwrap().is_err());
    }
    assert_eq!(fetches(&server).await, 1, "one attempt between them");
}
