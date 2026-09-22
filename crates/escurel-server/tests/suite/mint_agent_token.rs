//! `mint_agent_token` (knowledge-workbench backend P2-6 — BRD FR-M-3): the
//! gateway hands an interactive agent a run-bound bearer. The token names
//! the agent (`agent:<skill>`), keeps the human visible as the actor
//! (`act.sub`), carries the caller's OWN authority (never more: an admin's
//! mint is admin, a member's mint is their groups), and the run identity
//! claims the rest of the workbench backend keys on — so drafts made with
//! it are stamped, `report_progress` accepts it, and `list_events{run_id}`
//! shows the run. The gateway writes `run-started` at mint and
//! `run-finished { status: expired }` when the token lapses unused.
//!
//! Real gateway (test issuer, gateway signing on the issuer's key), real
//! DuckDB, raw JSON-RPC. No runner: the agent here is the test itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const SKILL: &str =
    "---\ntype: skill\nid: renewal\nautonomy: review\nvisibility: public\n---\n# renewal\n";
const PAGE_BODY: &str = "---\ntype: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";
/// An owner-private skill whose one instance is OWNED BY the agent
/// principal a member could mint (`agent:renewal`). The workbench token
/// must not confer that ownership on the human who minted it.
const VAULT_SKILL: &str =
    "---\ntype: skill\nid: vault\nvisibility: owner\nowner_field: credential\n---\n# vault\n";
const VAULT_PAGE_BODY: &str = "---\ntype: instance\nid: v1\nskill: vault\ncredential: \"agent:renewal\"\n---\n# v1\n\nSECRET-OF-THE-AGENT\n";
const VAULT_PAGE: &str = "markdown/instances/vault/v1.md";

async fn start(signing: bool) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            signing,
            minted_run_sweep: Some(Duration::from_millis(300)),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL)
                .skill("vault", VAULT_SKILL)
                .instance("renewal", "c1", PAGE_BODY)
                .instance("vault", "v1", VAULT_PAGE_BODY)
                .done(),
        ),
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn claims_of(token: &str) -> Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).expect("jwt");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("b64");
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn a_gateway_without_a_signing_key_refuses_to_mint() {
    let p = start(false).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let err = rpc(
        &p,
        &alice,
        "mint_agent_token",
        json!({ "skill": "renewal" }),
    )
    .await;
    assert_eq!(err["error"]["data"]["code"], "unsupported", "{err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ESCUREL_AUTH_SIGNING_KEY"),
        "{err}"
    );
}

#[tokio::test]
async fn a_minted_token_names_the_agent_keeps_the_human_and_carries_the_run() {
    let p = start(true).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let admin = p.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");

    let r = call(
        &p,
        &alice,
        "mint_agent_token",
        json!({ "skill": "renewal", "target_page_id": PAGE, "ttl_secs": 600 }),
    )
    .await;
    let token = r["token"].as_str().expect("token").to_owned();
    let run_id = r["run_id"].as_str().expect("run_id").to_owned();
    assert_eq!(r["subject"], "agent:renewal", "{r}");
    assert!(r["expires_at"].is_string(), "{r}");
    let claims = claims_of(&token);
    assert_eq!(claims["sub"], "agent:renewal", "{claims}");
    assert_eq!(
        claims["act"]["sub"], "alice",
        "the human stays visible: {claims}"
    );
    assert_eq!(claims["tenant"], TENANT);
    assert_eq!(claims["run_id"], run_id, "{claims}");
    assert_eq!(claims["purpose"], "workbench_agent", "{claims}");
    let roles = claims["roles"].as_array().unwrap();
    assert!(
        !roles.iter().any(|r| r == "escurel:admin"),
        "a member's mint is not admin: {claims}"
    );

    // The gateway accepts what it minted, and the run identity does its job:
    // a draft made with it is stamped, and report_progress takes it.
    let head = call(&p, &token, "expand", json!({ "page_id": PAGE })).await;
    let base = head["content_sha256"]
        .as_str()
        .expect("content_sha256")
        .to_owned();
    let d = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": "---\ntype: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n\n- renewed\n",
                "base_sha256": base }),
    )
    .await;
    assert_eq!(d["draft"]["run_id"], run_id, "{d}");
    let pr = call(
        &p,
        &token,
        "report_progress",
        json!({ "plan": [{ "step": "renew", "status": "in_progress" }] }),
    )
    .await;
    assert_eq!(pr["run_id"], run_id, "{pr}");

    // The token carries alice's authority, not the agent principal's: a page
    // owned by `agent:renewal` stays hidden from her minted token (codex
    // second-opinion review of P2, P1: the ACL subject is the human).
    let peek = rpc(&p, &token, "expand", json!({ "page_id": VAULT_PAGE })).await;
    assert!(!peek.to_string().contains("SECRET-OF-THE-AGENT"), "{peek}");
    assert!(
        peek["result"]["structuredContent"]["page"].is_null() || peek.get("error").is_some(),
        "{peek}"
    );

    // The run exists as a run: started at mint, on the target page, by alice.
    let own = call(&p, &admin, "list_events", json!({ "run_id": run_id })).await;
    let started = own["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["title"] == "run-started")
        .cloned()
        .unwrap_or_else(|| panic!("no run-started: {own}"));
    assert_eq!(started["instance_page_id"], PAGE, "{started}");
    assert_eq!(
        started["provenance"]["runner"]["harness"], "workbench",
        "{started}"
    );
    assert_eq!(
        started["provenance"]["runner"]["requested_by"], "alice",
        "{started}"
    );
    assert_eq!(
        started["provenance"]["runner"]["minted_by"], "gateway",
        "{started}"
    );

    // An admin's mint carries admin; a bad skill id does not mint.
    let r = call(
        &p,
        &admin,
        "mint_agent_token",
        json!({ "skill": "renewal" }),
    )
    .await;
    let claims = claims_of(r["token"].as_str().unwrap());
    assert_eq!(claims["act"]["sub"], "ops:jo");
    assert!(
        claims["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "escurel:admin"),
        "{claims}"
    );
    let err = rpc(
        &p,
        &alice,
        "mint_agent_token",
        json!({ "skill": "not a skill" }),
    )
    .await;
    assert_eq!(err["error"]["code"], -32602, "{err}");
}

#[tokio::test]
async fn a_lapsed_token_closes_its_run_as_expired() {
    let p = start(true).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let admin = p.mint_token(TENANT, Role::Admin);
    let r = call(
        &p,
        &alice,
        "mint_agent_token",
        json!({ "skill": "renewal", "ttl_secs": 1 }),
    )
    .await;
    let run_id = r["run_id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(10);
    let finished = loop {
        let own = call(&p, &admin, "list_events", json!({ "run_id": run_id })).await;
        if let Some(f) = own["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == "run-finished")
        {
            break f.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no run-finished after expiry: {own}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let body: Value = serde_json::from_str(finished["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["status"], "expired", "{body}");
}

/// Hardening H4: which minted runs are still open is derived from the
/// events themselves (`run-started` with `minted_by: gateway`, an
/// `expires_at` in the past, no `run-finished`), so a gateway restart does
/// not forget what its predecessor minted. Before H4 the sweep read an
/// in-memory list.
#[tokio::test]
async fn a_run_minted_before_a_restart_is_still_closed_as_expired() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/renewal.md", SKILL)
        .await
        .unwrap();
    let overrides = || ConfigOverrides {
        indexer: Some(Arc::clone(&indexer)),
        signing: true,
        minted_run_sweep: Some(Duration::from_millis(300)),
        ..Default::default()
    };

    // Gateway A mints and goes away before the token lapses.
    let a = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: overrides(),
        ..Default::default()
    })
    .await;
    let alice = a.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let r = call(
        &a,
        &alice,
        "mint_agent_token",
        json!({ "skill": "renewal", "ttl_secs": 1 }),
    )
    .await;
    let run_id = r["run_id"].as_str().unwrap().to_owned();
    a.shutdown().await;

    // Gateway B on the same store closes it once it has lapsed.
    let b = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: overrides(),
        ..Default::default()
    })
    .await;
    let admin = b.mint_token(TENANT, Role::Admin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let finished = loop {
        let own = call(&b, &admin, "list_events", json!({ "run_id": run_id })).await;
        if let Some(f) = own["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == "run-finished")
        {
            break f.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the restarted gateway never closed the run: {own}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let body: Value = serde_json::from_str(finished["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["status"], "expired", "{body}");
    // Swept once: a later sweep does not write a second terminal (the id is
    // first-writer-wins anyway), and nothing else is pending.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let own = call(&b, &admin, "list_events", json!({ "run_id": run_id })).await;
    assert_eq!(
        own["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["title"] == "run-finished")
            .count(),
        1
    );
}
