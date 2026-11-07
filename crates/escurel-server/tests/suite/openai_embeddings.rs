//! `ESCUREL_EMBEDDING_PROVIDER=alibaba | openai-compatible` against a real
//! local HTTP server speaking the OpenAI `/embeddings` shape, through the
//! real boot path. Chat and embedding providers are independent: `hetzner`
//! (a runner preset) is refused here because it serves no embeddings.

use std::collections::HashMap;

use escurel_embed::Embedder;
use escurel_server::EscurelConfig;
use escurel_server::config::EmbeddingProvider;
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "sk-test-not-a-real-credential";

fn source(pairs: Vec<(String, String)>) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs.into_iter().collect();
    move |k: &str| map.get(k).cloned()
}

/// One 768-dim vector per input, in order.
struct Vectors;
impl wiremock::Respond for Vectors {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).expect("json");
        let n = body["input"].as_array().map_or(0, Vec::len);
        let data: Vec<Value> = (0..n)
            .map(|i| json!({ "object": "embedding", "index": i, "embedding": vec![0.01_f32; 768] }))
            .collect();
        ResponseTemplate::new(200)
            .set_body_json(json!({ "object": "list", "data": data, "model": body["model"] }))
    }
}

struct Env {
    data: TempDir,
    key_file: std::path::PathBuf,
    _keys: TempDir,
}

fn env() -> Env {
    let keys = TempDir::new().unwrap();
    let key_file = keys.path().join("embedding-key");
    std::fs::write(&key_file, format!("{KEY}\n")).unwrap();
    Env {
        data: TempDir::new().unwrap(),
        key_file,
        _keys: keys,
    }
}

fn config(env: &Env, extra: &[(&str, String)]) -> Result<EscurelConfig, String> {
    let mut pairs = vec![
        (
            "ESCUREL_SERVER_DATA_DIR".to_owned(),
            env.data.path().to_str().unwrap().to_owned(),
        ),
        (
            "ESCUREL_SERVER_LISTEN_HTTP".to_owned(),
            "127.0.0.1:0".to_owned(),
        ),
        (
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN".to_owned(),
            "127.0.0.1:0".to_owned(),
        ),
        (
            "ESCUREL_REBUILD_INDEX_ON_BOOT".to_owned(),
            "if-missing".to_owned(),
        ),
        ("ESCUREL_EMBEDDER_REQUIRED".to_owned(), "true".to_owned()),
    ];
    pairs.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
    EscurelConfig::from_source(&source(pairs)).map_err(|e| e.to_string())
}

fn models_seen(reqs: &[Request]) -> Vec<String> {
    reqs.iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .filter_map(|b| b["model"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn hetzner_is_refused_as_an_embedding_provider_with_the_reason() {
    let env = env();
    let err =
        config(&env, &[("ESCUREL_EMBEDDING_PROVIDER", "hetzner".into())]).expect_err("refused");
    assert!(err.contains("no embeddings endpoint"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alibaba_without_a_key_file_does_not_boot_when_the_embedder_is_required() {
    let env = env();
    let cfg = config(&env, &[("ESCUREL_EMBEDDING_PROVIDER", "alibaba".into())]).unwrap();
    assert_eq!(cfg.embedding_provider, EmbeddingProvider::Alibaba);
    let Err(err) = cfg.build().await else {
        panic!("refused")
    };
    let err = err.to_string();
    assert!(
        err.contains("ESCUREL_EMBEDDING_OPENAI_API_KEY_FILE"),
        "{err}"
    );
}

/// Boots on alibaba (default model `text-embedding-v4`), then reboots the
/// SAME data directory on another model: the reboot re-embeds the corpus —
/// the second model is asked to embed pages it did not write — instead of
/// leaving the first model's vectors beside the second's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alibaba_embeds_with_bearer_auth_and_a_model_change_re_embeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/compatible-mode/v1/embeddings"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(Vectors)
        .mount(&server)
        .await;
    let env = env();
    let base = format!("{}/compatible-mode/v1", server.uri());
    let common = |model: Option<&str>| {
        let mut v = vec![
            ("ESCUREL_EMBEDDING_PROVIDER", "alibaba".to_owned()),
            ("ESCUREL_EMBEDDING_OPENAI_BASE_URL", base.clone()),
            (
                "ESCUREL_EMBEDDING_OPENAI_API_KEY_FILE",
                env.key_file.to_str().unwrap().to_owned(),
            ),
        ];
        if let Some(m) = model {
            v.push(("ESCUREL_EMBEDDING_MODEL", m.to_owned()));
        }
        v
    };

    {
        let booted = config(&env, &common(None))
            .unwrap()
            .build()
            .await
            .expect("boots");
        assert!(booted.embedder.is_loaded());
        assert_eq!(booted.embedder.model_id(), "alibaba:text-embedding-v4");
        booted.handle.shutdown().await;
    }
    let first = models_seen(&server.received_requests().await.unwrap());
    assert!(
        !first.is_empty() && first.iter().all(|m| m == "text-embedding-v4"),
        "the boot embedded the corpus on the default model: {first:?}"
    );
    let reqs = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["dimensions"], 768);

    let booted = config(&env, &common(Some("text-embedding-v3")))
        .unwrap()
        .build()
        .await
        .expect("reboots");
    assert_eq!(booted.embedder.model_id(), "alibaba:text-embedding-v3");
    booted.handle.shutdown().await;
    let all = models_seen(&server.received_requests().await.unwrap());
    let second: Vec<&String> = all[first.len()..].iter().collect();
    assert!(
        !second.is_empty() && second.iter().all(|m| *m == "text-embedding-v3"),
        "a model change must re-embed the existing corpus at boot: {second:?}"
    );
}

/// A model switch whose provider refuses (a rotated key, say) does not take
/// the tenant down: the boot serves the recorded vectors unless the operator
/// required the embedder, in which case it refuses to boot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_re_embed_at_boot_is_fatal_only_when_the_embedder_is_required() {
    let good = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Vectors)
        .mount(&good)
        .await;
    let refusing = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Incorrect API key"))
        .mount(&refusing)
        .await;
    let env = env();
    let with = |server: &MockServer, model: &str, required: &str| {
        vec![
            ("ESCUREL_EMBEDDING_PROVIDER", "alibaba".to_owned()),
            (
                "ESCUREL_EMBEDDING_OPENAI_BASE_URL",
                format!("{}/compatible-mode/v1", server.uri()),
            ),
            (
                "ESCUREL_EMBEDDING_OPENAI_API_KEY_FILE",
                env.key_file.to_str().unwrap().to_owned(),
            ),
            ("ESCUREL_EMBEDDING_MODEL", model.to_owned()),
            ("ESCUREL_EMBEDDER_REQUIRED", required.to_owned()),
        ]
    };
    {
        let booted = config(&env, &with(&good, "text-embedding-v4", "true"))
            .unwrap()
            .build()
            .await
            .expect("first boot");
        booted.handle.shutdown().await;
    }
    let Err(err) = config(&env, &with(&refusing, "text-embedding-v3", "true"))
        .unwrap()
        .build()
        .await
    else {
        panic!("a required embedder that cannot re-embed refuses the boot")
    };
    let err = err.to_string();
    assert!(err.contains("401"), "{err}");
    let booted = config(&env, &with(&refusing, "text-embedding-v3", "false"))
        .unwrap()
        .build()
        .await
        .expect("not required: boots on the recorded vectors");
    assert!(
        !booted.embedder.is_loaded() && booted.embedder.model_id() == "zero",
        "the new model must not serve over the old model's vectors"
    );
    booted.handle.shutdown().await;
}
