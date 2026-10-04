//! A supplier-risk run under review leaves its ANALYSIS beside its change to the order.
//!
//! Real gateway, real runner (minted mode), real echo harness. The run drafts the order's note AND a
//! `supplier-risk-analysis` instance in the SAME changeset; the instance carries typed fields, the
//! takeaway sentence and the table behind the chart (its text alternative), and the order links to it.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const RISK: &str = "---\nkind: skill\nid: supplier-risk\nautonomy: review\nactions:\n  - {name: write-analysis, kind: event, label: Write the analysis, event: supplier-risk-analysis}\n  - {name: update-order, kind: event, label: Update the order, event: customer-order}\n---\n# supplier-risk\n";
const ANALYSIS_SKILL: &str = "---\nkind: skill\nid: supplier-risk-analysis\nautonomy: review\n---\n# supplier-risk-analysis\n";
const ORDER_SKILL: &str =
    "---\nkind: skill\nid: customer-order\nautonomy: review\n---\n# customer-order\n";
const SUPPLIER_SKILL: &str = "---\nkind: skill\nid: supplier\nautonomy: review\n---\n# supplier\n";

fn order(id: &str, customer: &str, qty: u32, net: &str) -> String {
    format!(
        "---\nkind: instance\nid: {id}\nskill: customer-order\nsold_to_name: {customer}\ncurrency: EUR\n---\n# {id}\n\n| Item | Material | Description | Qty | Unit | Net value | Confirmed |\n|---|---|---|---:|---|---:|---|\n| 10 | GH-4711 | Gearbox | {qty} | PC | {net} | 2026-10-12 |\n"
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
async fn a_supplier_risk_run_drafts_an_analysis_in_the_same_changeset_as_its_fold() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("supplier-risk", RISK)
                .skill("supplier-risk-analysis", ANALYSIS_SKILL)
                .skill("customer-order", ORDER_SKILL)
                .skill("supplier", SUPPLIER_SKILL)
                .instance("supplier", "meier-guss", "---\nkind: instance\nid: meier-guss\nskill: supplier\nvendor: 100234\nname: Meier-Guss GmbH\n---\n# Meier-Guss GmbH\n")
                .instance("customer-order", "order-1", order("order-1", "Hoffmann GmbH", 240, "62,400.00"))
                .instance("customer-order", "order-2", order("order-2", "Kessler GmbH", 200, "66,200.00"))
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

    call(&gw, &alice, "capture_event", json!({
        "source": "test", "mime": "text/plain", "label_skill": "supplier-risk",
        "instance_page_id": "markdown/instances/customer-order/order-1.md",
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
            "expected the fold and the analysis as open drafts: {d}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    // One changeset holds both drafts: one promotion publishes the change and its analysis.
    assert_eq!(
        drafts[0]["changeset_id"], drafts[1]["changeset_id"],
        "{drafts:?}"
    );
    let analysis = drafts
        .iter()
        .find(|d| {
            d["target_page_id"]
                .as_str()
                .unwrap()
                .contains("supplier-risk-analysis__")
        })
        .expect("an analysis draft");
    let fold = drafts
        .iter()
        .find(|d| {
            d["target_page_id"]
                .as_str()
                .unwrap()
                .contains("customer-order")
        })
        .expect("the order draft");
    let content = analysis["content"].as_str().unwrap();

    for line in [
        "risk_level: high",
        "risk_score: 90",
        "orders_affected: 2",
        "net_value_at_risk: 128600.00",
        "supplier: \"[[supplier::meier-guss]]\"",
    ] {
        assert!(content.contains(line), "missing `{line}` in\n{content}");
    }
    // The chart's text alternative: the takeaway as a sentence, and the table it is drawn from.
    assert!(content.contains("2 orders are affected and carry 128,600.00 EUR of net value; the largest, order-2 (Kessler GmbH), is 51% of it."), "{content}");
    assert!(
        content.contains("| Order | Customer | Qty | Net value (EUR) | Share |"),
        "{content}"
    );
    // The order links to its analysis.
    let id = analysis["target_page_id"]
        .as_str()
        .unwrap()
        .rsplit("__")
        .next()
        .unwrap()
        .trim_end_matches(".md");
    // A human id: the supplier and the day the signal arrived, not a random tail of an event id.
    let day_id = id.strip_prefix("meier-guss-").unwrap_or_default();
    assert!(
        day_id.len() == 10
            && day_id
                .chars()
                .enumerate()
                .all(|(i, c)| if i == 4 || i == 7 {
                    c == '-'
                } else {
                    c.is_ascii_digit()
                }),
        "expected meier-guss-YYYY-MM-DD, got {id}"
    );
    // The page says which signal it answers in words; the event id stays as provenance only.
    assert!(content.contains("**Triggered by** "), "{content}");
    assert!(content.contains("source_event: "), "{content}");
    assert!(
        fold["content"]
            .as_str()
            .unwrap()
            .contains(&format!("[[supplier-risk-analysis::{id}]]")),
        "{}",
        fold["content"]
    );
}

#[tokio::test]
async fn a_second_run_for_the_same_supplier_and_day_gets_its_own_analysis_id_while_the_first_is_still_a_draft()
 {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("supplier-risk", RISK)
                .skill("supplier-risk-analysis", ANALYSIS_SKILL)
                .skill("customer-order", ORDER_SKILL)
                .skill("supplier", SUPPLIER_SKILL)
                .instance("supplier", "meier-guss", "---\nkind: instance\nid: meier-guss\nskill: supplier\nvendor: 100234\nname: Meier-Guss GmbH\n---\n# Meier-Guss GmbH\n")
                .instance("customer-order", "order-1", order("order-1", "Hoffmann GmbH", 240, "62,400.00"))
                .instance("customer-order", "order-2", order("order-2", "Kessler GmbH", 200, "66,200.00"))
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

    let capture = |order: &str, n: u32| {
        json!({
            "source": "test", "mime": "text/plain", "label_skill": "supplier-risk",
            "instance_page_id": format!("markdown/instances/customer-order/{order}.md"),
            "title": format!("Vendor 100234 Meier-Guss: signal {n}"),
            "body": "vendor 100234, material GH-4711. Confirmation moved (+14 days); vendor rating downgraded A to B.",
        })
    };
    // The first signal: its fold and its analysis are open drafts nobody has decided.
    call(&gw, &alice, "capture_event", capture("order-1", 1)).await;
    open_drafts(&gw, &alice, 2).await;
    // The second signal, same supplier, same day, while the first analysis is still only a DRAFT (so no
    // stored page holds its id yet): it must pick its own id, not collide and dead-letter.
    call(&gw, &alice, "capture_event", capture("order-2", 2)).await;
    let drafts = open_drafts(&gw, &alice, 4).await;

    let mut ids: Vec<&str> = drafts
        .iter()
        .filter_map(|d| d["target_page_id"].as_str())
        .filter(|t| t.contains("supplier-risk-analysis__"))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        2,
        "two runs, two distinct analysis pages: {ids:?}"
    );
}

async fn open_drafts(gw: &EscurelProcess, alice: &str, want: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let d = call(gw, alice, "list_drafts", json!({})).await;
        let open: Vec<Value> = d["drafts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["status"] == "open")
            .cloned()
            .collect();
        if open.len() >= want {
            return open;
        }
        assert!(
            Instant::now() < deadline,
            "expected {want} open drafts (the run must not have dead-lettered): {d}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
