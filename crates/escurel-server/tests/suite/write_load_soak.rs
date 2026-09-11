//! #431 — sustained `update_page` load must not take the writer down.
//!
//! Lab, 2026-08-30: `dz-escurel` died with container exit **139** — SIGSEGV —
//! inside a seeding burst of paced `update_page` writes.
//!
//! This test reproduces the same defect deterministically in about two seconds
//! of load, and it does not need the lab: drive ordinary page writes through
//! the real gateway and the write path stops returning. The stack under the
//! stuck `tx.commit()` ends in the `vss` extension —
//! `usearch::index_dense_gt::remove` — and the minimal repro needs no escurel
//! at all: an HNSW index created on an empty table blocks for ever on the
//! 192nd delete-then-insert cycle, and segfaults instead if the index was
//! rebuilt along the way. Every `update_page` does exactly one such cycle
//! (`materialise::replace_blocks`), and `Migrator::up` creates that index on a
//! fresh, empty DuckDB. See
//! `docs/notes/discovered/2026-09-11-vss-hnsw-churn-hangs-then-segfaults.md`.
//!
//! It boots the index through `SingleFileStore::open` — the same code path the
//! binary boots, HNSW and all — rather than the harness default, which gives
//! the CRDT backend its own DuckDB file and never enables the vector index.
//! That default is why no existing test in this suite could have found this:
//! its own comment says the production clone "is not reachable from here".
//!
//! **`#[ignore]` until #431 is fixed**, for two reasons. The first is that a
//! test known to fail earns nothing by running. The second is worse and worth
//! stating: the per-request timeout below does panic with a clear message, but
//! the *process still cannot exit* — a runtime worker is parked inside
//! `libduckdb` for ever, so the harness waits on a thread that will never
//! return and the run has to be killed from outside. Run it by hand with
//! `cargo test -p escurel-server --test suite write_load_soak -- --ignored`,
//! and delete the attribute in the commit that fixes the index.
//!
//! Scale is env-tunable: `ESCUREL_SOAK_PAGES` (default 24),
//! `ESCUREL_SOAK_REVISIONS` (default 6) and `ESCUREL_SOAK_BURST` (default 4,
//! `1` for a strictly sequential writer — the hang needs no concurrency).

use std::sync::Arc;
use std::time::Duration;

use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::ContextualizeMode;
use escurel_index::snapshot::{IndexStore, SingleFileStore};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;

const TENANT: &str = "acme";
const CUSTOMER: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

fn env_count(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn page_id(i: usize) -> String {
    format!("markdown/instances/customer/c{i}.md")
}

/// A body with enough shape to make the CRDT do real work: a changing
/// paragraph, a growing list, and a frontmatter field that moves every time.
fn body(i: usize, rev: usize) -> String {
    let bullets: String = (0..=rev)
        .map(|n| format!("- note {n} for customer {i}\n"))
        .collect();
    format!(
        "---\ntype: instance\nskill: customer\nid: c{i}\nrev: {rev}\n---\n\
         # Customer {i}\n\nRevision {rev} of a page that keeps being rewritten.\n\n\
         {bullets}\nTrailing paragraph {rev}, so the tail moves too.\n"
    )
}

/// One MCP call, bounded. A write that has not answered in 30s is not slow —
/// the observed failure never answers at all — and a test that hangs tells a
/// CI run nothing except that it stopped.
async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(30), call_inner(p, name, args))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{name} did not answer within 30s — #431: the write path stops \
                 returning inside `tx.commit()`, in the vss HNSW index"
            )
        })
}

async fn call_inner(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("the gateway must still be accepting requests")
        .json()
        .await
        .expect("the gateway must still be answering JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reproduces #431: the vss HNSW index hangs the write path; \
            remove this attribute in the commit that fixes it"]
async fn sustained_update_page_load_does_not_take_the_writer_down() {
    let pages = env_count("ESCUREL_SOAK_PAGES", 24);
    let revisions = env_count("ESCUREL_SOAK_REVISIONS", 6);
    // How many writes are in flight at once. 1 makes the load strictly
    // sequential, which is how you tell a concurrency bug from a
    // data-dependent one.
    let burst = env_count("ESCUREL_SOAK_BURST", 4).max(1);

    // Production's own boot sequence: one DuckDB instance, `vss`/`fts`
    // loaded, HNSW persistence on, and the CRDT connection `try_clone`d off
    // the indexer's rather than opened on a second file.
    let state_dir = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let opened = SingleFileStore {
        tenant_dir: state_dir.path().join("tenants").join(TENANT),
        rebuild_on_boot: false,
        store: Arc::clone(&store),
        embedder,
        tenant: TENANT.to_owned(),
        contextualize: ContextualizeMode::default(),
        attach_retrieval: None,
        seed_dir: None,
    }
    .open()
    .await
    .expect("boot the index the way the binary does");
    let crdt_conn = opened
        .crdt_conn
        .expect("SingleFileStore hands back the cloned CRDT connection");
    let crdt_backend: Arc<dyn CrdtBackend> =
        Arc::new(DuckdbCrdtBackend::new(Arc::new(Mutex::new(crdt_conn))));

    let mut fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("customer", CUSTOMER);
    for i in 0..pages {
        fixtures = fixtures.instance("customer", &format!("c{i}"), body(i, 0).as_str());
    }
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures.done()),
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&opened.indexer)),
            crdt_backend: Some(crdt_backend),
            ..Default::default()
        },
    })
    .await;

    // Revise every page, round after round. Each round writes the pages in
    // bursts of `burst` concurrent requests — the seeder's own shape, and the
    // one that puts two writes inside the CRDT at the same time.
    let mut accepted = 0usize;
    for rev in 1..=revisions {
        for start in (0..pages).step_by(burst) {
            let calls = (start..(start + burst).min(pages)).map(|i| {
                let p = &process;
                async move {
                    (
                        i,
                        call(
                            p,
                            "update_page",
                            json!({ "page_id": page_id(i), "content": body(i, rev) }),
                        )
                        .await,
                    )
                }
            });
            for (i, env) in futures::future::join_all(calls).await {
                let out = &env["result"]["structuredContent"];
                assert_eq!(
                    out["ok"], true,
                    "write of page {i} rev {rev} must be accepted: {env}"
                );
                accepted += 1;
            }
        }
    }

    // The premise: this test is worthless if the load never happened.
    assert_eq!(
        accepted,
        pages * revisions,
        "every write must have been attempted and accepted"
    );

    // Still serving, and the content is the last revision — not a version
    // string that advanced past content that never landed.
    for i in 0..pages {
        let out = call(&process, "expand", json!({ "page_id": page_id(i) })).await;
        let content = out["result"]["structuredContent"]["content"]
            .as_str()
            .unwrap_or_else(|| panic!("page {i} must still be readable: {out}"));
        assert!(
            content.contains(&format!("Revision {revisions} of a page")),
            "page {i} must hold the last revision written, not an earlier one"
        );
    }

    process.shutdown().await;
}
