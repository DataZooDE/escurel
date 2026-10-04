//! `/ingest` materialises a document instance without going through `update_page`, so it must apply the
//! same autonomy gate: an upload by a MACHINE (a run token) into a document skill that asks for review
//! does not land. It built its ACL caller with no run / actor / skill claims, so a machine looked like a
//! person (and a narrowed token's skill confinement was not applied either).
//!
//! Real gateway + DuckDB + verifier, born-digital text.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

fn doc_skill(id: &str, autonomy: &str) -> String {
    format!(
        "---\nkind: skill\nid: {id}\ndescription: {id}.\nautonomy: {autonomy}\nowner_field: author\nacl:\n  read: [owner]\n  create: [owner]\n\
         backend:\n  kind: document\n  accepts: [text/plain]\n---\n# {id}\n"
    )
}

async fn post_ingest(
    p: &EscurelProcess,
    token: &str,
    blob: &str,
    skill: &str,
) -> (reqwest::StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "blob_id": blob, "content_type": "text/plain", "skill": skill }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or_default())
}

#[tokio::test]
async fn a_machine_upload_into_a_review_document_skill_is_refused_a_person_s_lands() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    for (id, a) in [("reviewed", "review"), ("open", "auto")] {
        indexer
            .update_page(&format!("markdown/skills/{id}.md"), &doc_skill(id, a))
            .await
            .unwrap();
    }
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let deposit = |body: &'static str| {
        let store = Arc::clone(&store);
        async move {
            store
                .put_inbox_blob(TENANT, Bytes::from(body.as_bytes().to_vec()), None)
                .await
                .unwrap()
                .as_str()
                .to_owned()
        }
    };
    let machine = p.mint_token_for_run(TENANT, Role::Agent, "agent:ingest", "run-1", "root-1");
    let machine_admin =
        p.mint_token_for_run(TENANT, Role::Admin, "agent:ingest", "run-2", "root-2");
    let person = p.mint_token(TENANT, Role::Agent);

    let b1 = deposit("one").await;
    let (st, body) = post_ingest(&p, &machine, &b1, "reviewed").await;
    assert_eq!(st, 409, "a machine's upload into a review skill: {body}");
    assert_eq!(body["error"], "review_required", "{body}");
    let b2 = deposit("two").await;
    let (st, body) = post_ingest(&p, &machine_admin, &b2, "reviewed").await;
    assert_eq!(st, 409, "an admin RUN token is a machine too: {body}");
    // Nothing was materialised.
    let docs = indexer
        .list_instances("reviewed", None, None, None, None, None)
        .await
        .unwrap();
    assert!(docs.is_empty(), "{docs:?}");

    // `auto` lands for a machine; a person lands in the review skill.
    let b3 = deposit("three").await;
    let (st, body) = post_ingest(&p, &machine, &b3, "open").await;
    assert!(st.is_success(), "{st}: {body}");
    let b4 = deposit("four").await;
    let (st, body) = post_ingest(&p, &person, &b4, "reviewed").await;
    assert!(st.is_success(), "{st}: {body}");
    p.shutdown().await;
}
