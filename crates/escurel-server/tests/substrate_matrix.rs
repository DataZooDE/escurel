//! The five-substrate measurement harness behind §6.7 of `docs/paper`.
//!
//! One logical entity — a customer — described by five skills, one per
//! backend kind, and the *same* navigation sequence run against each:
//! `resolve` → `expand` → `neighbours`, plus a `search` probe for discovery.
//!
//! Everything external is real: a Postgres in a container for `sql_view`, the
//! repository's `report.pdf` through `/ingest` for `document`, an in-process
//! axum CRM for `openapi`, and an in-process JSON-RPC server for `mcp`. No
//! doubles at the boundary this measurement exists to cover.
//!
//! **What is under test, recorded before the run.** The claim in §3.3 is that
//! *navigation* is identical across substrates. It is already conceded that
//! *discovery* is not — the two live proxies are never indexed, so similarity
//! search cannot reach them. Counting "tool calls to answer" would be
//! tautological here, because this harness fixes the sequence itself; so the
//! measured quantity is whether each step of that fixed sequence **succeeds**
//! against each backend, with discovery reported separately. If a navigation
//! step fails for any reason other than the conceded search limitation, the
//! corollary is weaker than §3.3 claims and §3.3 is what changes.
//!
//! Emits JSON to `$ESCUREL_PAPER_DATA` (default: stdout) for
//! `docs/paper/data/substrates.json`.
//!
//! Opt-in (needs Docker):
//! `cargo test -p escurel-server --features live-substrates --test substrate_matrix -- --nocapture`
#![cfg(feature = "live-substrates")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio::net::TcpListener;
use tokio_postgres::NoTls;

const TENANT: &str = "acme";
/// Samples per latency cell. p95 of 50 is the 48th order statistic, which is
/// as much resolution as this many samples honestly supports.
const SAMPLES: usize = 50;
/// The markdown instance's page id is its path in the corpus.
const MD_PAGE: &str = "markdown/instances/customer_md/acme.md";

// --- the five skills, one per backend kind -----------------------------

const MD_SKILL: &str = "\
---
type: skill
id: customer_md
description: Customers held as native markdown.
---
# customer_md
";

fn sql_skill() -> String {
    "\
---
type: skill
id: customer_sql
description: Customers mirrored read-only from the CRM database.
backend:
  kind: sql_view
  source: { connector: postgres, attach: crm_pg, relation: public.customers }
  project: { display_name: name, tier: tier }
  search_text: [name]
---
# customer_sql
"
    .to_owned()
}

const DOC_SKILL: &str = "\
---
type: skill
id: customer_doc
description: Customer account reviews ingested as documents.
backend:
  kind: document
  accepts: [application/pdf]
  chunk: { max_chars: 400, overlap: 40 }
---
# customer_doc
";

const REST_SKILL: &str = "\
---
type: skill
id: customer_rest
description: Customers proxied live over the CRM REST API.
backend:
  kind: openapi
  endpoint: crm_rest
  read: { path: \"/customers/{id}\" }
  write: { method: PATCH, path: \"/customers/{id}\" }
  project: { display_name: $.name, tier: $.account_tier }
---
# customer_rest
";

const MCP_SKILL: &str = "\
---
type: skill
id: customer_mcp
description: Customers proxied live from the upstream MCP server.
backend:
  kind: mcp
  endpoint: crm_mcp
  read: { tool: getCustomer }
  write: { tool: putCustomer }
  project: { display_name: $.name, tier: $.tier }
---
# customer_mcp
";

// --- real upstreams ----------------------------------------------------

type Db = Arc<Mutex<std::collections::BTreeMap<String, Value>>>;

fn seed_db() -> Db {
    Arc::new(Mutex::new(
        [(
            "acme".to_owned(),
            json!({ "id": "acme", "name": "Acme Corp", "account_tier": "gold", "tier": "gold" }),
        )]
        .into_iter()
        .collect(),
    ))
}

async fn rest_get(Path(id): Path<String>, State(db): State<Db>) -> Json<Value> {
    Json(db.lock().unwrap().get(&id).cloned().unwrap_or(Value::Null))
}

async fn rest_patch(
    Path(id): Path<String>,
    State(db): State<Db>,
    Json(patch): Json<Value>,
) -> Json<Value> {
    let mut g = db.lock().unwrap();
    let row = g.entry(id).or_insert_with(|| json!({}));
    if let (Some(obj), Some(p)) = (row.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            obj.insert(k.clone(), v.clone());
        }
    }
    Json(row.clone())
}

async fn mcp_rpc(State(db): State<Db>, Json(req): Json<Value>) -> Json<Value> {
    let id = req.get("id").cloned().unwrap_or(json!(1));
    let method = req["method"].as_str().unwrap_or_default();
    let params = &req["params"];
    let result = match method {
        "tools/list" => json!({
            "tools": [ { "name": "getCustomer" }, { "name": "putCustomer" } ]
        }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let args = &params["arguments"];
            let cid = args["id"].as_str().unwrap_or_default().to_owned();
            let mut g = db.lock().unwrap();
            let row = g.entry(cid).or_insert_with(|| json!({}));
            if name == "putCustomer"
                && let (Some(obj), Some(p)) = (row.as_object_mut(), args.as_object())
            {
                for (k, v) in p {
                    if k != "id" {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
            json!({ "structuredContent": row.clone(), "content": [], "isError": false })
        }
        other => json!({ "error": format!("unknown method {other}") }),
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

// --- gateway + call helpers --------------------------------------------

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Admin);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

/// A tool call "succeeds" when the envelope carries no JSON-RPC error and the
/// structured result does not report `ok: false`.
fn ok(v: &Value) -> bool {
    v.get("error").is_none() && v["result"]["structuredContent"]["ok"] != json!(false)
}

fn sc(v: &Value) -> &Value {
    &v["result"]["structuredContent"]
}

/// Each navigation step gets a specific success criterion, not merely the
/// absence of an error: `resolve` must report the target exists, `expand`
/// must return a page (a body or frontmatter), `neighbours` must return a
/// link set. A tool that answers 200 with nothing in it has not navigated.
fn resolved_ok(v: &Value) -> bool {
    ok(v) && sc(v)["exists"] == json!(true)
}
fn expanded_ok(v: &Value) -> bool {
    ok(v) && (!sc(v)["body"].is_null() || !sc(v)["frontmatter"].is_null())
}
fn neighbours_ok(v: &Value) -> bool {
    ok(v) && sc(v).get("edges").is_some_and(|e| e.is_array())
}

/// Bytes of the structured payload the agent would carry into its context.
fn payload_bytes(v: &Value) -> usize {
    serde_json::to_string(&v["result"]["structuredContent"])
        .map(|s| s.len())
        .unwrap_or(0)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// Time `expand` `SAMPLES` times and return (p50, p95) in milliseconds.
async fn expand_latency(p: &EscurelProcess, page_id: &str) -> (f64, f64) {
    let mut ms = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t = Instant::now();
        let _ = call(p, "expand", json!({ "page_id": page_id })).await;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (percentile(&ms, 0.50), percentile(&ms, 0.95))
}

/// Run the fixed navigation sequence and record what each step did.
async fn measure(
    p: &EscurelProcess,
    backend: &str,
    skill_id: &str,
    wikilink: &str,
    page_id: &str,
) -> Value {
    let resolved = call(p, "resolve", json!({ "wikilink": wikilink })).await;
    let expanded = call(p, "expand", json!({ "page_id": page_id })).await;
    let neighbours = call(
        p,
        "neighbours",
        json!({ "page_id": page_id, "direction": "both" }),
    )
    .await;
    // Discovery is read from the skill's declared capabilities, NOT probed with
    // a similarity query. This harness runs a zero-vector embedder, under which
    // every vector is identical and `search` returns essentially the whole
    // corpus -- a probe would have reported that live backends are findable,
    // which is the opposite of the truth and an artefact of the fixture.
    let skills = call(p, "list_skills", json!({})).await;
    let searchable = sc(&skills)["skills"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|sk| sk["id"] == json!(skill_id))
                .map(|sk| sk["capabilities"]["search"].clone())
        })
        .unwrap_or(Value::Null);

    let (p50, p95) = expand_latency(p, page_id).await;

    json!({
        "backend": backend,
        "resolve_ok": resolved_ok(&resolved),
        "expand_ok": expanded_ok(&expanded),
        "neighbours_ok": neighbours_ok(&neighbours),
        "search_capability": searchable,
        "payload_bytes": payload_bytes(&expanded),
        "expand_p50_ms": (p50 * 100.0).round() / 100.0,
        "expand_p95_ms": (p95 * 100.0).round() / 100.0,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn substrate_matrix_measures_all_five_backends() {
    // --- real upstreams -------------------------------------------------
    let pg = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn =
        format!("host=127.0.0.1 port={pg_port} user=postgres password=postgres dbname=postgres");
    let (pg_client, pg_conn) = tokio_postgres::connect(&dsn, NoTls)
        .await
        .expect("pg connect");
    tokio::spawn(async move {
        let _ = pg_conn.await;
    });
    pg_client
        .batch_execute(
            "CREATE TABLE public.customers (id TEXT PRIMARY KEY, name TEXT, tier TEXT);
             INSERT INTO public.customers VALUES ('acme', 'Acme Corp', 'gold');",
        )
        .await
        .expect("seed pg");

    let db = seed_db();
    let (rest_url, _rest) = serve(
        Router::new()
            .route("/customers/{id}", get(rest_get).patch(rest_patch))
            .with_state(Arc::clone(&db)),
    )
    .await;
    let (mcp_url, _mcp) = serve(
        Router::new()
            .route("/mcp", post(mcp_rpc))
            .with_state(Arc::clone(&db)),
    )
    .await;

    // --- gateway seeded with all five skills ----------------------------
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    for (id, md) in [
        ("customer_md", MD_SKILL.to_owned()),
        ("customer_sql", sql_skill()),
        ("customer_doc", DOC_SKILL.to_owned()),
        ("customer_rest", REST_SKILL.to_owned()),
        ("customer_mcp", MCP_SKILL.to_owned()),
    ] {
        indexer
            .update_page(&format!("markdown/skills/{id}.md"), &md)
            .await
            .unwrap();
    }
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let p = &process;

    // --- one instance per substrate -------------------------------------
    // Every instance is created *before* any measurement, so the `search`
    // probe sees the same corpus for all five. Measuring as we went would
    // have given the substrate created first a smaller index to be found in.

    // 1. markdown
    indexer
        .update_page(
            MD_PAGE,
            "---\ntype: instance\nskill: customer_md\nid: acme\n---\n\
             # Acme Corp\n\nTier gold. The account is in good standing.\n",
        )
        .await
        .unwrap();

    // 2. sql_view over the live Postgres
    let cred = call(
        p,
        "register_credential",
        json!({ "name": "crm_pg", "connector": "postgres", "secret": dsn }),
    )
    .await;
    assert!(ok(&cred), "register_credential: {cred}");
    let sql_inst = call(
        p,
        "create_sql_instance",
        json!({ "skill": "customer_sql", "id": "acme" }),
    )
    .await;
    assert!(ok(&sql_inst), "create_sql_instance: {sql_inst}");
    let sql_page = sc(&sql_inst)["page_id"]
        .as_str()
        .expect("sql page_id")
        .to_owned();

    // 3. document — the repository's real PDF, uploaded as a blob then ingested
    let pdf = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/report.pdf"),
    )
    .expect("report.pdf fixture");
    let blob = store
        .put_inbox_blob(TENANT, Bytes::from(pdf), None)
        .await
        .expect("put blob");
    let token = p.mint_token(TENANT, Role::Admin);
    let ingested: Value = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "blob_id": blob.as_str(),
            "content_type": "application/pdf",
            "skill": "customer_doc",
            "title": "Acme Corp account review",
        }))
        .send()
        .await
        .expect("ingest")
        .json()
        .await
        .expect("ingest json");
    assert_eq!(ingested["status"], "materialised", "ingest: {ingested}");
    let doc_page = ingested["page_id"].as_str().expect("page_id").to_owned();

    // 4/5. the two live proxies
    let mcp_endpoint = format!("{mcp_url}/mcp");
    for (name, kind, url) in [
        ("crm_rest", "openapi", rest_url.as_str()),
        ("crm_mcp", "mcp", mcp_endpoint.as_str()),
    ] {
        let reg = call(
            p,
            "register_endpoint",
            json!({ "name": name, "kind": kind, "base_url": url }),
        )
        .await;
        assert!(ok(&reg), "register_endpoint {name}: {reg}");
    }
    let mut remote_pages = Vec::new();
    for skill in ["customer_rest", "customer_mcp"] {
        let created = call(
            p,
            "create_remote_instance",
            json!({ "skill": skill, "id": "acme" }),
        )
        .await;
        assert!(ok(&created), "create_remote_instance {skill}: {created}");
        remote_pages.push(
            sc(&created)["page_id"]
                .as_str()
                .expect("page_id")
                .to_owned(),
        );
    }

    // The lexical lane is refreshed once, after every instance exists, so the
    // discovery probe is run against one corpus rather than five.
    indexer.refresh_fts().await.unwrap();

    // --- measure --------------------------------------------------------
    let doc_ref = {
        let ex = call(p, "expand", json!({ "page_id": doc_page })).await;
        let fm = &ex["result"]["structuredContent"]["frontmatter"];
        format!(
            "[[{}::{}]]",
            fm["skill"].as_str().unwrap_or("customer_doc"),
            fm["id"].as_str().unwrap_or(&doc_page)
        )
    };
    let mut cells = Vec::new();
    for (backend, skill_id, wikilink, page_id) in [
        (
            "markdown",
            "customer_md",
            "[[customer_md::acme]]".to_owned(),
            MD_PAGE.to_owned(),
        ),
        (
            "sql_view",
            "customer_sql",
            "[[customer_sql::acme]]".to_owned(),
            sql_page,
        ),
        ("document", "customer_doc", doc_ref, doc_page.clone()),
        (
            "openapi",
            "customer_rest",
            "[[customer_rest::acme]]".to_owned(),
            remote_pages[0].clone(),
        ),
        (
            "mcp",
            "customer_mcp",
            "[[customer_mcp::acme]]".to_owned(),
            remote_pages[1].clone(),
        ),
    ] {
        cells.push(measure(p, backend, skill_id, &wikilink, &page_id).await);
    }

    let out = json!({
        "samples_per_latency_cell": SAMPLES,
        "note": "resolve/expand/neighbours is the fixed navigation sequence. \
                 Discovery is read from declared capabilities, not probed: the \
                 fixture runs a zero-vector embedder. The openapi/mcp upstreams \
                 are in-process on loopback, so their latency is a FLOOR that \
                 excludes real network round-trip time.",
        "embedder": "zero",
        "upstreams": "loopback, in-process",
        "cells": cells,
    });
    let rendered = serde_json::to_string_pretty(&out).unwrap();
    match std::env::var("ESCUREL_PAPER_DATA") {
        Ok(path) => std::fs::write(&path, &rendered).expect("write paper data"),
        Err(_) => println!("{rendered}"),
    }

    // The corollary, asserted rather than only reported: the fixed navigation
    // sequence must succeed on every substrate. Discovery is *not* asserted --
    // the two live proxies are never indexed, and that limit is conceded in
    // the paper rather than measured away here.
    for c in &cells {
        assert_eq!(c["resolve_ok"], json!(true), "resolve failed: {c}");
        assert_eq!(c["expand_ok"], json!(true), "expand failed: {c}");
        assert_eq!(c["neighbours_ok"], json!(true), "neighbours failed: {c}");
    }
}
