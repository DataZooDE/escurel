//! The vocabulary a skill page may carry to place and describe itself (OKF alignment, stage 2):
//! `folder:` (a `/` path), `role:` (record|process|report|helper), `tags:`, and the optional OKF keys
//! `title`, `resource`, `generated`, `verified`, `status`, `stale_after`, `sources`.
//! `list_skills` reports what a skill declares; `validate` errors on a bad `folder`/`role` and only
//! WARNS about the OKF keys, never rejecting an unknown key. Real gateway, real DuckDB.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "okf";
const ORDER: &str = "---\nkind: skill\nid: customer-order\ndescription: d.\nautonomy: review\n\
summary: A customer order.\nfolder: sales/orders\nrole: record\ntags: [sap, sd]\ntitle: Customer order\n\
resource: https://sap.example/vbak\n---\n# customer-order\n";
const CHECKED: &str = "---\nkind: skill\nid: analysis\ndescription: d.\nautonomy: review\nsummary: s.\n\
generated: agent:supplier-risk\nverified: 2026-09-30\nstatus: draft\nstale_after: P90D\n\
sources: [https://sap.example/doc, {title: SAP VBAK, url: https://sap.example/vbak}]\n\
viewer: {report: supplier-risk-report, param: analysis}\n---\n# analysis\n";
const PLAIN: &str =
    "---\nkind: skill\nid: plain\ndescription: d.\nautonomy: review\nsummary: s.\n---\n# plain\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer-order", ORDER)
                .skill("plain", PLAIN)
                .skill("analysis", CHECKED)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn skill(extra: &str) -> String {
    format!(
        "---\nkind: skill\nid: note\ndescription: d.\nautonomy: review\nsummary: s.\n{extra}---\n# note\n"
    )
}

fn issue<'a>(out: &'a Value, code: &str) -> Option<&'a Value> {
    out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["code"] == code)
}

#[tokio::test]
async fn list_skills_reports_folder_role_tags_title_and_resource() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(&p, &token, "list_skills", json!({})).await;
    let rows = out["skills"].as_array().unwrap();
    let find = |id: &str| {
        rows.iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("{id}: {out}"))
    };
    let o = find("customer-order");
    assert_eq!(o["folder"], "sales/orders", "{o}");
    assert_eq!(o["role"], "record", "{o}");
    assert_eq!(o["tags"], json!(["sap", "sd"]), "{o}");
    assert_eq!(o["title"], "Customer order", "{o}");
    assert_eq!(o["resource"], "https://sap.example/vbak", "{o}");
    // A skill that declares none of them carries none of them: the rows stay as they were.
    let plain = find("plain");
    for key in ["folder", "role", "tags", "title", "resource"] {
        assert!(plain.get(key).is_none(), "{key} on a plain skill: {plain}");
    }
}

#[tokio::test]
async fn a_bad_folder_or_role_is_an_error_and_a_good_one_is_clean() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let good = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill(
        "folder: sales/orders\nrole: process\ntags: [a, b/c]\n") }),
    )
    .await;
    for code in ["folder_invalid", "role_unknown", "tags_invalid"] {
        assert!(
            issue(&good, code).is_none(),
            "{code} on a good skill: {good}"
        );
    }
    assert_eq!(good["ok"], true, "{good}");

    for folder in [
        "/sales",
        "sales/",
        "sales//orders",
        "Sales/Orders",
        "sales/or ders",
        "sales/../x",
    ] {
        let out = call(
            &p,
            &token,
            "validate",
            json!({ "content": skill(&format!("folder: \"{folder}\"\n")) }),
        )
        .await;
        let i = issue(&out, "folder_invalid").unwrap_or_else(|| panic!("{folder}: {out}"));
        assert_eq!(i["severity"], "error", "{out}");
        assert_eq!(i["location"], "frontmatter.folder", "{out}");
        assert_eq!(out["ok"], false, "{folder}: {out}");
    }
    // Not a string at all.
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("folder: [a, b]\n") }),
    )
    .await;
    assert!(issue(&out, "folder_invalid").is_some(), "{out}");

    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("role: gadget\n") }),
    )
    .await;
    let i = issue(&out, "role_unknown").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["severity"], "error", "{out}");
    assert!(
        i["suggestion"].as_str().unwrap_or("").contains("record"),
        "{out}"
    );
    assert_eq!(out["ok"], false);
}

#[tokio::test]
async fn the_okf_keys_only_warn_and_unknown_keys_are_never_rejected() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // Well-formed OKF keys: nothing to say. An RFC3339 instant and an ISO-8601 duration are both fine.
    for stale in ["2027-01-01T00:00:00Z", "P90D"] {
        let out = call(&p, &token, "validate", json!({ "content": skill(&format!(
            "title: T\nresource: https://x.example\ngenerated: 2026-10-01T10:00:00Z\nverified: 2026-10-02T10:00:00Z\n\
status: draft\nstale_after: {stale}\nsources: [https://a.example, https://b.example]\nx-anything: 1\n")) })).await;
        assert_eq!(out["ok"], true, "{stale}: {out}");
        let bad: Vec<_> = out["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["severity"] != "info")
            .collect();
        assert!(bad.is_empty(), "{stale}: {out}");
    }

    // Malformed ones are WARNINGS: ok stays true, the author is told.
    for (extra, code, location) in [
        ("tags: sap\n", "tags_invalid", "frontmatter.tags"),
        (
            "stale_after: someday\n",
            "stale_after_invalid",
            "frontmatter.stale_after",
        ),
        ("sources: one\n", "sources_invalid", "frontmatter.sources"),
        (
            "verified: not-a-date\n",
            "verified_invalid",
            "frontmatter.verified",
        ),
        (
            "generated: yesterday\n",
            "generated_invalid",
            "frontmatter.generated",
        ),
    ] {
        let out = call(&p, &token, "validate", json!({ "content": skill(extra) })).await;
        let i = issue(&out, code).unwrap_or_else(|| panic!("{code}: {out}"));
        assert_eq!(i["severity"], "warning", "{out}");
        assert_eq!(i["location"], location, "{out}");
        assert_eq!(out["ok"], true, "a warning does not fail validation: {out}");
    }

    // The vocabulary is for SKILL pages: an instance keeps its own meaning for `status` and `tags`.
    let inst =
        "---\nkind: instance\nskill: plain\nid: i1\nstatus: open\ntags: not-a-list\n---\n# i1\n";
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": inst, "as_page_id": "markdown/instances/plain/i1.md" }),
    )
    .await;
    for code in ["tags_invalid", "stale_after_invalid", "sources_invalid"] {
        assert!(issue(&out, code).is_none(), "{code} on an instance: {out}");
    }
}

#[tokio::test]
async fn list_skills_reports_the_okf_provenance_keys_and_the_viewer() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(&p, &token, "list_skills", json!({})).await;
    let rows = out["skills"].as_array().unwrap();
    let find = |id: &str| {
        rows.iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("{id}: {out}"))
    };
    let a = find("analysis");
    assert_eq!(a["generated"], "agent:supplier-risk", "{a}");
    assert_eq!(a["verified"], "2026-09-30", "{a}");
    assert_eq!(a["status"], "draft", "{a}");
    assert_eq!(a["stale_after"], "P90D", "{a}");
    assert_eq!(
        a["sources"],
        json!(["https://sap.example/doc", {"title": "SAP VBAK", "url": "https://sap.example/vbak"}]),
        "{a}"
    );
    // Peacock's `viewer:` rides along so a client can say where a skill's instances are charted.
    assert_eq!(
        a["viewer"],
        json!({"report": "supplier-risk-report", "param": "analysis"}),
        "{a}"
    );
    // Declared nothing, carries nothing: the other rows stay byte-identical to before.
    let plain = find("plain");
    for key in [
        "generated",
        "verified",
        "status",
        "stale_after",
        "sources",
        "viewer",
    ] {
        assert!(plain.get(key).is_none(), "{key} on a plain skill: {plain}");
    }
}
