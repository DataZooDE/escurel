//! A supplier-risk run over ROW instances: the orders and the supplier are `instances: rows`
//! sql_views over SAP-shaped extracts, and the agent writes THROUGH THE MARKDOWN SIDE — its fold of
//! the order lands in the order's linked markdown, never in the row, and it must not carry the row's
//! source columns (the gateway refuses a write that does: `backend_read_only_field`).
//!
//! Real gateway, real DuckDB reading real JSON files, real runner (minted mode), real echo harness.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const RISK: &str = "---\nkind: skill\nid: supplier-risk\nautonomy: review\n---\n# supplier-risk\n";
const ANALYSIS_SKILL: &str = "---\nkind: skill\nid: supplier-risk-analysis\nautonomy: review\n---\n# supplier-risk-analysis\n";

fn order_skill(dir: &str) -> String {
    format!(
        "---\nkind: skill\nid: customer-order\nautonomy: review\nbackend:\n  kind: sql_view\n  instances: rows\n  key: order_id\n  linked: markdown\n  source: {{connector: json_dir, relation: \"{dir}\"}}\n  project: {{name1: sold_to_name, waerk: currency, netwr: net_value}}\n---\n# customer-order\n"
    )
}
fn supplier_skill(dir: &str) -> String {
    format!(
        "---\nkind: skill\nid: supplier\nautonomy: review\nbackend:\n  kind: sql_view\n  instances: rows\n  key: supplier_id\n  linked: markdown\n  filterable: [lifnr]\n  source: {{connector: json_dir, relation: \"{dir}\"}}\n  project: {{lifnr: vendor, name1: name}}\n---\n# supplier\n"
    )
}

/// The order's linked markdown: its notes and items (NOT the row's columns).
fn companion(id: &str, qty: u32, net: &str) -> String {
    format!(
        "---\nkind: instance\nid: {id}\nskill: customer-order\ndelivery_risk: low\n---\n# {id}\n\n| Item | Material | Description | Qty | Unit | Net value | Confirmed |\n|---|---|---|---:|---|---:|---|\n| 10 | GH-4711 | Gearbox | {qty} | PC | {net} | 2026-10-12 |\n"
    )
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } }))
        .send().await.expect("post").json().await.expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn a_run_over_row_instances_drafts_into_the_markdown_side_and_never_the_row() {
    let src = tempfile::tempdir().unwrap();
    let orders = src.path().join("vbak");
    let suppliers = src.path().join("lfa1");
    std::fs::create_dir_all(&orders).unwrap();
    std::fs::create_dir_all(&suppliers).unwrap();
    for (id, customer, net) in [
        ("order-1", "Hoffmann GmbH", 62_400.0),
        ("order-2", "Kessler GmbH", 66_200.0),
    ] {
        std::fs::write(
            orders.join(format!("{id}.json")),
            json!({ "order_id": id, "name1": customer, "waerk": "EUR", "netwr": net }).to_string(),
        )
        .unwrap();
    }
    std::fs::write(
        suppliers.join("meier-guss.json"),
        json!({ "supplier_id": "meier-guss", "lifnr": 100_234, "name1": "Meier-Guss GmbH" })
            .to_string(),
    )
    .unwrap();

    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("supplier-risk", RISK)
                .skill("supplier-risk-analysis", ANALYSIS_SKILL)
                .skill("customer-order", order_skill(orders.to_str().unwrap()))
                .skill("supplier", supplier_skill(suppliers.to_str().unwrap()))
                .instance(
                    "customer-order",
                    "order-1",
                    companion("order-1", 240, "62,400.00"),
                )
                .instance(
                    "customer-order",
                    "order-2",
                    companion("order-2", 200, "66,200.00"),
                )
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");

    let (signing_key, kid) = gw.signing_material();
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", gw.issuer_url())
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    let order_page = "markdown/instances/customer-order/order-1.md";
    call(&gw, &alice, "capture_event", json!({
        "source": "test", "mime": "text/plain", "label_skill": "supplier-risk",
        "instance_page_id": order_page,
        "title": "Vendor 100234 Meier-Guss: PO 1 confirmation moved +14 days",
        "body": "vendor 100234, material GH-4711. Confirmation moved (+14 days); vendor rating downgraded A to B.",
    })).await;

    let deadline = Instant::now() + Duration::from_secs(40);
    let drafts = loop {
        let d = call(&gw, &alice, "list_drafts", json!({})).await;
        let open: Vec<Value> = d["drafts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["status"] == "open")
            .cloned()
            .collect();
        if open.len() >= 2 {
            break open;
        }
        assert!(
            Instant::now() < deadline,
            "expected the order's fold and the analysis as open drafts: {d}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(
        drafts[0]["changeset_id"], drafts[1]["changeset_id"],
        "{drafts:?}"
    );

    let fold = drafts
        .iter()
        .find(|d| d["target_page_id"] == order_page)
        .expect("a draft against the order's linked markdown");
    let content = fold["content"].as_str().unwrap();
    // The write carries the companion's own fields, never the row's source columns.
    for source_column in ["sold_to_name", "currency", "net_value"] {
        assert!(
            !content.contains(&format!("{source_column}:")),
            "the draft must not carry the source column `{source_column}`:\n{content}"
        );
    }
    assert!(content.contains("delivery_risk: low"), "{content}");
    assert!(content.contains("folded event"), "{content}");

    // One promotion of the changeset publishes both; the ROW is exactly as the source has it.
    let changeset = fold["changeset_id"].as_str().unwrap();
    let promoted = call(
        &gw,
        &alice,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(promoted["ok"], true, "{promoted}");
    let page = call(&gw, &alice, "expand", json!({ "page_id": order_page })).await;
    assert_eq!(
        page["frontmatter"]["sold_to_name"], "Hoffmann GmbH",
        "{page}"
    );
    assert!(
        page["body"].as_str().unwrap().contains("folded event"),
        "the note landed in the companion: {page}"
    );
    assert_eq!(
        page["backend_projection"]["rows"][0]["netwr"].as_f64(),
        Some(62_400.0)
    );
}
