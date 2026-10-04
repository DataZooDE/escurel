//! The directory connectors (`json_dir`, `parquet_dir`) read ANY file the gateway can read, so they are
//! confined to the directories the operator exposes (`ESCUREL_SQL_FILE_DIRS`) — exactly as a SQLite
//! file is. A tenant admin who points a skill at `/etc/passw*`, `/proc/self/environ*` or another
//! tenant's data got rows (or a parser error quoting the file) back to any caller who could `expand`.
//!
//! Real gateway, real DuckDB, real files in real temp directories; the policy is the operator's.

use std::path::Path;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::egress::EgressPolicy;
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const MARKER: &str = "TOP-SECRET-OUTSIDE-MARKER";

fn skill(id: &str, glob: &str) -> String {
    format!(
        "---\nkind: skill\nid: {id}\ndescription: {id}.\nfields:\n  - {{name: ident, kind: string, required: true}}\n  - {{name: note, kind: string}}\n\
         backend:\n  kind: sql_view\n  instances: rows\n  key: ident\n  linked: markdown\n  \
         source: {{connector: json_dir, relation: \"{glob}\"}}\n  project: {{ident: ident, note: note}}\n---\n# {id}\n"
    )
}

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header(
            "authorization",
            format!("Bearer {}", p.mint_token(TENANT, role)),
        )
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn write_rows(dir: &Path, note: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("rows.json"),
        serde_json::to_string(&json!([{ "ident": "r1", "note": note }])).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn a_directory_connector_reads_only_what_the_operator_exposes() {
    let exposed = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write_rows(&exposed.path().join("ok"), "inside-ok");
    write_rows(outside.path(), MARKER);
    // A link INSIDE the exposed directory that leads out.
    write_rows(&outside.path().join("linked"), MARKER);
    std::fs::create_dir_all(exposed.path().join("bad")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("linked"),
        exposed.path().join("bad/out"),
    )
    .unwrap();

    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    let mut policy = EgressPolicy::default();
    policy.sql_file_dirs = vec![exposed.path().to_path_buf()];
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            egress: Some(policy),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    let cases = [
        ("outside", format!("{}/*.json", outside.path().display())),
        ("passwd", "/etc/passw*".to_owned()),
        ("environ", "/proc/self/environ*".to_owned()),
        (
            "dotdot",
            format!("{}/ok/../*.json", exposed.path().display()),
        ),
        (
            "linked",
            format!("{}/bad/**/*.json", exposed.path().display()),
        ),
    ];
    for (id, glob) in cases {
        call(
            &p,
            Role::Admin,
            "update_page",
            json!({ "page_id": format!("markdown/skills/{id}.md"), "content": skill(id, &glob) }),
        )
        .await;
        // A non-admin reads: this is the exfiltration path.
        for tool in [
            ("list_instances", json!({ "skill": id, "limit": 5 })),
            (
                "expand",
                json!({ "page_id": format!("markdown/instances/{id}/r1.md") }),
            ),
        ] {
            let out = call(&p, Role::Agent, tool.0, tool.1).await.to_string();
            assert!(
                !out.contains(MARKER) && !out.contains("root:") && !out.contains("PATH="),
                "{id}/{}: file contents left the exposed directory: {out}",
                tool.0
            );
        }
    }

    // The control: an exposed directory still reads.
    call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/ok.md",
                "content": skill("ok", &format!("{}/ok/*.json", exposed.path().display())) }),
    )
    .await;
    let ok = call(
        &p,
        Role::Agent,
        "list_instances",
        json!({ "skill": "ok", "limit": 5 }),
    )
    .await
    .to_string();
    assert!(ok.contains("r1"), "an exposed directory reads: {ok}");
}
