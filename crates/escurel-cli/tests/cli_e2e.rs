//! End-to-end tests for the `escurel` CLI (agent surface).
//!
//! Spins up the real gateway via `escurel-test-support` and drives the
//! compiled binary (`assert_cmd::cargo_bin`). No mocks at the CLI
//! boundary; the support crate's in-process JWKS issuer stands in for a
//! real OIDC realm. Every command + its common switches are exercised,
//! plus both `--format` modes and the auth on/off paths.

use assert_cmd::Command;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::Value;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
optional_frontmatter: [tier]\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: gold\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";

const INITECH_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: initech\n\
name: Initech\n\
---\n\
# Initech\n";

struct Harness {
    process: EscurelProcess,
    http_addr: String,
    bearer: String,
}

async fn start() -> Harness {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            ..Default::default()
        },
    })
    .await;
    let http_addr = process
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    let bearer = process.mint_token(TENANT, Role::Agent);
    Harness {
        process,
        http_addr,
        bearer,
    }
}

/// Build a CLI command pre-wired with server + token env, running the
/// given args on the blocking pool (assert_cmd is sync).
async fn run_args(h: &Harness, args: Vec<String>) -> std::process::Output {
    let addr = h.http_addr.clone();
    let bearer = h.bearer.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{addr}"))
            .env("ESCUREL_TOKEN", bearer)
            .args(&args)
            .assert()
            .success()
            .get_output()
            .clone()
    })
    .await
    .unwrap()
}

async fn run_stdin(h: &Harness, args: Vec<String>, stdin: &str) -> std::process::Output {
    let addr = h.http_addr.clone();
    let bearer = h.bearer.clone();
    let stdin = stdin.to_owned();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{addr}"))
            .env("ESCUREL_TOKEN", bearer)
            .args(&args)
            .write_stdin(stdin)
            .assert()
            .success()
            .get_output()
            .clone()
    })
    .await
    .unwrap()
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

fn v(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

async fn acme_page_id(h: &Harness) -> String {
    let out = run_args(h, v(&["instance", "list", "--skill", "customer"])).await;
    let inst = json(&out);
    inst["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["page_id"].as_str().unwrap().contains("acme"))
        .unwrap()["page_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

// --- read / browse -------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_list_emits_seeded_skill() {
    let h = start().await;
    let out = run_args(&h, v(&["skill", "list"])).await;
    let val = json(&out);
    assert!(
        val["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "customer")
    );
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn instance_list_honours_switches() {
    let h = start().await;
    // --skill + --order-by-at + --limit together.
    let out = run_args(
        &h,
        v(&[
            "instance",
            "list",
            "--skill",
            "customer",
            "--order-by-at",
            "desc",
            "--limit",
            "1",
        ]),
    )
    .await;
    let val = json(&out);
    assert_eq!(val["instances"].as_array().unwrap().len(), 1);
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_emits_existing_page() {
    let h = start().await;
    let out = run_args(&h, v(&["resolve", "[[customer::acme]]"])).await;
    let val = json(&out);
    assert_eq!(val["exists"], true);
    assert_eq!(val["page"]["skill"], "customer");
    assert_eq!(val["page"]["slug"], "acme");
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_snapshots_emits_snapshot_array() {
    let h = start().await;
    let page_id = acme_page_id(&h).await;
    let out = run_args(&h, v(&["page", "snapshots", &page_id])).await;
    let val = json(&out);
    // A fixture-seeded instance has no CRDT session history yet, so the
    // list is present and empty — the point is that the command is wired
    // and returns the real (array) shape.
    assert!(
        val["snapshots"].is_array(),
        "snapshots must be an array: {val}"
    );
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_expand_emits_body_and_wikilinks() {
    let h = start().await;
    let page_id = acme_page_id(&h).await;
    let out = run_args(&h, v(&["page", "expand", &page_id])).await;
    let val = json(&out);
    assert!(val["body"].as_str().unwrap().contains("Acme Corp"));
    assert!(
        val["wikilinks_out"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == "initech")
    );
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn link_neighbours_traverses() {
    let h = start().await;
    let page_id = acme_page_id(&h).await;
    // direction + limit switches.
    let out = run_args(
        &h,
        v(&[
            "link",
            "neighbours",
            &page_id,
            "--direction",
            "out",
            "--limit",
            "10",
        ]),
    )
    .await;
    let val = json(&out);
    assert!(val["edges"].is_array());
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_honours_switches_and_table_format() {
    let h = start().await;
    // --k + --page-type + --skill.
    let out = run_args(
        &h,
        v(&[
            "search",
            "Acme",
            "--k",
            "5",
            "--page-type",
            "any",
            "--skill",
            "customer",
        ]),
    )
    .await;
    let val = json(&out);
    assert!(!val["hits"].as_array().unwrap().is_empty());
    assert_eq!(val["granularity"], "block");

    // Same query, table format: human output, non-JSON, mentions a hit.
    let table = run_args(&h, v(&["--format", "table", "search", "Acme", "--k", "5"])).await;
    let text = String::from_utf8_lossy(&table.stdout);
    assert!(text.contains("hits:"), "table output should label hits");
    assert!(serde_json::from_slice::<Value>(&table.stdout).is_err());
    h.process.shutdown().await;
}

// --- write ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_validate_accepts_well_formed_body() {
    let h = start().await;
    let page_id = acme_page_id(&h).await;
    let out = run_stdin(&h, v(&["page", "validate", &page_id]), ACME_INSTANCE).await;
    let val = json(&out);
    assert_eq!(val["ok"], true, "issues: {:?}", val["issues"]);
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_update_via_stdin_round_trips() {
    let h = start().await;
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: globex\n\
                name: Globex\n\
                ---\n\
                # Globex\n";
    let out = run_stdin(
        &h,
        v(&["page", "update", "markdown/instances/customer/globex.md"]),
        body,
    )
    .await;
    assert_eq!(json(&out)["ok"], true);
    h.process.shutdown().await;
}

// --- events (M7 CRM core) ------------------------------------------

/// The realistic CRM flow end to end through the CLI:
/// capture → inbox → assign → list on the instance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_capture_inbox_assign_list_flow() {
    let h = start().await;
    let acme = acme_page_id(&h).await;

    // capture (body via --body), with common switches set.
    let captured = run_args(
        &h,
        v(&[
            "event",
            "capture",
            "--title",
            "Renewal call",
            "--body",
            "Acme wants to renew.",
            "--source",
            "manual",
            "--label-skill",
            "note",
        ]),
    )
    .await;
    let event_id = json(&captured)["event_id"].as_str().unwrap().to_owned();
    assert!(!event_id.is_empty());

    // inbox shows it (with --limit).
    let inbox = run_args(&h, v(&["event", "inbox", "--limit", "50"])).await;
    assert!(
        json(&inbox)["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_id"] == event_id)
    );

    // assign it to acme.
    let assigned = run_args(
        &h,
        v(&["event", "assign", "--event", &event_id, "--instance", &acme]),
    )
    .await;
    assert_eq!(json(&assigned)["instance_page_id"], acme);

    // list the instance's processed history (with --limit).
    let hist = run_args(
        &h,
        v(&["event", "list", "--instance", &acme, "--limit", "10"]),
    )
    .await;
    let found = json(&hist)["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["event_id"] == event_id && e["status"] == "processed");
    assert!(found, "assigned event should be in processed history");
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_capture_reads_body_from_stdin() {
    let h = start().await;
    let out = run_stdin(
        &h,
        v(&["event", "capture", "--title", "piped"]),
        "piped event body",
    )
    .await;
    let val = json(&out);
    assert_eq!(val["body"], "piped event body");
    assert_eq!(val["status"], "inbox");
    h.process.shutdown().await;
}

// --- chat ----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_append_then_list_round_trips() {
    let h = start().await;
    for (ts, content) in [
        ("2026-05-26T10:00:00Z", "hi"),
        ("2026-05-26T10:00:05Z", "there"),
    ] {
        run_args(
            &h,
            v(&[
                "chat",
                "append",
                "--group",
                "room-1",
                "--role",
                "user",
                "--content",
                content,
                "--ts",
                ts,
            ]),
        )
        .await;
    }
    // list with --direction + --limit switches.
    let out = run_args(
        &h,
        v(&[
            "chat",
            "list",
            "--group",
            "room-1",
            "--direction",
            "asc",
            "--limit",
            "100",
        ]),
    )
    .await;
    let bodies: Vec<String> = json(&out)["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(bodies, vec!["hi".to_owned(), "there".to_owned()]);
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_append_reads_content_from_stdin() {
    let h = start().await;
    let out = run_stdin(
        &h,
        v(&[
            "chat",
            "append",
            "--group",
            "room-2",
            "--ts",
            "2026-05-26T10:00:00Z",
        ]),
        "piped body",
    )
    .await;
    assert_eq!(json(&out)["msg_id"].as_str().unwrap().len(), 26);
    let list = run_args(
        &h,
        v(&["chat", "list", "--group", "room-2", "--direction", "asc"]),
    )
    .await;
    assert_eq!(json(&list)["messages"][0]["content"], "piped body");
    h.process.shutdown().await;
}

// --- layer / shadows (REQ-LAYER-03/04) ------------------------------

/// `skill list` must surface each skill's `layer` (+ the `shadows` pin
/// when a tenant overlay shadows a pack base skill), and `page expand`
/// must print the server's `shadow` drift object — the CLI half of the
/// pack-layer visibility contract.
///
/// Base pages are seeded through a test-owned `Indexer` +
/// `seed_from_dir` (the same canonical import path a pack import uses),
/// NOT `FixtureBuilder`: fixtures replay through `update_page`, which
/// rejects `layer: base@…` (see `layer_read_only.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_list_and_page_expand_surface_layer_and_shadow() {
    use std::sync::Arc;

    const BASE_SKILL: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Firm-authored canonical procedure (v7).\n\
layer: base@logistics-midmarket@v7\n\
---\n\
# pallet-consolidation\n\nFirm-authored body.\n";
    const OVERLAY_SKILL: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Acme-specialised procedure.\n\
---\n\
# pallet-consolidation\n\nTenant-specialised body.\n";
    const PLAIN_SKILL: &str = "---\n\
type: skill\n\
id: local-notes\n\
description: Tenant-authored notes skill.\n\
---\n\
# local-notes\n";

    let store_dir = tempfile::TempDir::new().unwrap();
    let db_dir = tempfile::TempDir::new().unwrap();
    let seed_dir = tempfile::TempDir::new().unwrap();
    let base_skills = seed_dir.path().join("base/logistics-midmarket/skills");
    std::fs::create_dir_all(&base_skills).unwrap();
    std::fs::create_dir_all(seed_dir.path().join("skills")).unwrap();
    std::fs::write(base_skills.join("pallet-consolidation.md"), BASE_SKILL).unwrap();
    std::fs::write(
        seed_dir.path().join("skills/pallet-consolidation.md"),
        OVERLAY_SKILL,
    )
    .unwrap();
    std::fs::write(seed_dir.path().join("skills/local-notes.md"), PLAIN_SKILL).unwrap();

    let store: Arc<dyn escurel_storage::LaneStore> = Arc::new(escurel_storage::FsStore::new(
        store_dir.path().to_path_buf(),
    ));
    let embedder: Arc<dyn escurel_embed::Embedder> =
        Arc::new(escurel_embed::ZeroEmbedder::default());
    let conn = duckdb::Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    escurel_index::Migrator::up(&conn).unwrap();
    let indexer = Arc::new(escurel_index::Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer.seed_from_dir(seed_dir.path()).await.unwrap();

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let http_addr = process
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    let run = |args: Vec<String>| {
        let addr = http_addr.clone();
        tokio::task::spawn_blocking(move || {
            Command::cargo_bin("escurel")
                .unwrap()
                .env("ESCUREL_SERVER", format!("http://{addr}"))
                .env_remove("ESCUREL_TOKEN")
                .args(&args)
                .assert()
                .success()
                .get_output()
                .clone()
        })
    };

    // skill list: the shadowing overlay carries layer + the base pin;
    // the plain tenant skill carries layer only (shadows omitted).
    let out = run(v(&["skill", "list"])).await.unwrap();
    let skills = json(&out)["skills"].as_array().unwrap().clone();
    let shadowing = skills
        .iter()
        .find(|s| s["id"] == "pallet-consolidation")
        .expect("overlay skill listed");
    assert_eq!(shadowing["layer"], "overlay", "{skills:?}");
    assert_eq!(
        shadowing["shadows"], "base@logistics-midmarket@v7",
        "{skills:?}"
    );
    let plain = skills
        .iter()
        .find(|s| s["id"] == "local-notes")
        .expect("plain skill listed");
    assert_eq!(plain["layer"], "overlay", "{skills:?}");
    assert!(
        plain.get("shadows").is_none(),
        "shadows must be omitted when null: {plain}"
    );

    // page expand of the shadowing overlay: the `shadow` drift object
    // (base page id + pin + base frontmatter) must be printed.
    let out = run(v(&[
        "page",
        "expand",
        "markdown/skills/pallet-consolidation.md",
    ]))
    .await
    .unwrap();
    let val = json(&out);
    assert_eq!(
        val["frontmatter"]["description"],
        "Acme-specialised procedure."
    );
    assert_eq!(
        val["shadow"]["base_page_id"],
        "markdown/base/logistics-midmarket/skills/pallet-consolidation.md",
        "{val}"
    );
    assert_eq!(
        val["shadow"]["pack"], "base@logistics-midmarket@v7",
        "{val}"
    );
    assert_eq!(
        val["shadow"]["base"]["description"], "Firm-authored canonical procedure (v7).",
        "base value visible, not silently masked: {val}"
    );

    // A non-shadowing page prints no shadow key (additive field).
    let out = run(v(&["page", "expand", "markdown/skills/local-notes.md"]))
        .await
        .unwrap();
    assert!(
        json(&out).get("shadow").is_none(),
        "no shadow object for a plain page: {}",
        json(&out)
    );

    process.shutdown().await;
}

// --- auth modes ----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthenticated_mode_works_without_token() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            ..Default::default()
        },
    })
    .await;
    let http_addr = process
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    let out = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{http_addr}"))
            .env_remove("ESCUREL_TOKEN")
            .args(["skill", "list"])
            .assert()
            .success()
            .get_output()
            .clone()
    })
    .await
    .unwrap();
    assert!(
        json(&out)["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "customer")
    );
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_token_against_authed_server_emits_json_error() {
    let h = start().await;
    let addr = h.http_addr.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{addr}"))
            .env_remove("ESCUREL_TOKEN")
            .args(["skill", "list"])
            .assert()
            .failure()
            .get_output()
            .clone()
    })
    .await
    .unwrap();
    // JSON-on-stderr error contract: parseable object with `error`.
    // A missing bearer against an auth-enabled gateway is rejected at
    // the HTTP layer (401), which the CLI surfaces as an `http 401`
    // error carrying the `unauthorized` body.
    let err: Value = serde_json::from_slice(&out.stderr).expect("stderr is JSON");
    let msg = err["error"].as_str().unwrap().to_lowercase();
    assert!(
        msg.contains("401") || msg.contains("unauthorized"),
        "got: {err}"
    );
    h.process.shutdown().await;
}
