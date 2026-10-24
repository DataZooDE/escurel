//! A draft or a changeset discard may carry a lease fence: `lease: {page_id,
//! holder}`. A skill declares which fields make a page a lease (`lease:
//! {holder, expires, while?}`); the write is stored only while that page
//! still names `holder`, is unexpired and, if declared, in its active state.
//! The check runs under the page-write gate, so a takeover that updates the
//! lease page cannot interleave with a stale worker's write.
//!
//! Real gateway, real DuckDB, raw JSON-RPC. Every refusal is paired with the
//! accepted control that proves the fence is not simply refusing everything.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "leases";
const JOB_SKILL: &str = "---\nkind: skill\nid: job\ndescription: A unit of work with a lease.\n\
    visibility: public\n\
    lease:\n  holder: attempt\n  expires: lease_until\n  while: {status: running}\n---\n# job\n";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
// A page of a skill that declares no lease.
const PLAIN: &str = "---\nkind: instance\nskill: note\nid: plain\nattempt: h1\nlease_until: 99999999999\nstatus: running\n---\n# Plain\n";
const JOB_PAGE: &str = "markdown/instances/job/j1.md";
const TARGET: &str = "markdown/instances/note/target.md";

fn job(holder: &str, until: u64, status: &str) -> String {
    format!(
        "---\nkind: instance\nskill: job\nid: j1\nattempt: {holder}\nlease_until: {until}\nstatus: {status}\n---\n# Job\n"
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("job", JOB_SKILL)
                .skill("note", NOTE_SKILL)
                .instance("job", "j1", job("h1", now() + 600, "running"))
                .instance("note", "plain", PLAIN)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":tool,"arguments":args}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.get("error").is_none(), "{tool} error: {body}");
    body["result"]["structuredContent"].clone()
}

fn draft(lease_page: &str, holder: &str, id: &str) -> Value {
    json!({
        "target_page_id": format!("markdown/instances/note/{id}.md"),
        "content": format!("---\nkind: instance\nskill: note\nid: {id}\n---\n# {id}\n"),
        "base_sha256": "",
        "new_changeset": true,
        "lease": {"page_id": lease_page, "holder": holder},
    })
}

fn refused(r: &Value) -> bool {
    r["ok"] == false
        && r["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|x| x["code"] == "lease_fence"))
}

async fn set_job(p: &EscurelProcess, token: &str, content: String) {
    let head = call(p, token, "expand", json!({"page_id": JOB_PAGE})).await;
    let r = call(
        p,
        token,
        "update_page",
        json!({
            "page_id": JOB_PAGE, "content": content, "base_sha256": head["content_sha256"],
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
}

#[tokio::test]
async fn a_draft_is_stored_only_while_its_holder_owns_the_lease() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);

    let current = call(&p, &t, "create_draft", draft(JOB_PAGE, "h1", "a")).await;
    assert_eq!(current["ok"], true, "the holder may draft: {current}");

    // Takeover: the lease page now names h2.
    set_job(&p, &t, job("h2", now() + 600, "running")).await;
    let stale = call(&p, &t, "create_draft", draft(JOB_PAGE, "h1", "b")).await;
    assert!(refused(&stale), "{stale}");
    let listed = call(&p, &t, "list_drafts", json!({})).await;
    assert_eq!(
        listed["drafts"].as_array().unwrap().len(),
        1,
        "no draft for the stale holder: {listed}"
    );
    let fresh = call(&p, &t, "create_draft", draft(JOB_PAGE, "h2", "c")).await;
    assert_eq!(fresh["ok"], true, "{fresh}");
}

#[tokio::test]
async fn an_expired_inactive_missing_or_undeclared_lease_refuses() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);
    set_job(&p, &t, job("h1", now() - 5, "running")).await;
    assert!(
        refused(&call(&p, &t, "create_draft", draft(JOB_PAGE, "h1", "a")).await),
        "expired"
    );
    set_job(&p, &t, job("h1", now() + 600, "finished")).await;
    assert!(
        refused(&call(&p, &t, "create_draft", draft(JOB_PAGE, "h1", "b")).await),
        "inactive"
    );
    assert!(
        refused(
            &call(
                &p,
                &t,
                "create_draft",
                draft("markdown/instances/job/nope.md", "h1", "c")
            )
            .await
        ),
        "missing page"
    );
    assert!(
        refused(
            &call(
                &p,
                &t,
                "create_draft",
                draft("markdown/instances/note/plain.md", "h1", "d")
            )
            .await
        ),
        "a skill that declares no lease is not a lease"
    );
    // CONTROL: restoring a current lease lets the same holder through.
    set_job(&p, &t, job("h1", now() + 600, "running")).await;
    assert_eq!(
        call(&p, &t, "create_draft", draft(JOB_PAGE, "h1", "e")).await["ok"],
        true
    );
}

#[tokio::test]
async fn a_stale_holder_cannot_discard_its_changeset_either() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);
    let d = call(&p, &t, "create_draft", draft(JOB_PAGE, "h1", "a")).await;
    let cs = d["draft"]["changeset_id"].as_str().unwrap().to_owned();
    set_job(&p, &t, job("h2", now() + 600, "running")).await;
    let r = call(
        &p,
        &t,
        "discard_changeset",
        json!({
            "changeset_id": cs, "reason": "stale",
            "lease": {"page_id": JOB_PAGE, "holder": "h1"},
        }),
    )
    .await;
    assert!(refused(&r), "{r}");
    let listed = call(&p, &t, "list_drafts", json!({})).await;
    assert_eq!(listed["drafts"][0]["status"], "open", "{listed}");
    // CONTROL: the current holder may.
    let r = call(
        &p,
        &t,
        "discard_changeset",
        json!({
            "changeset_id": cs, "reason": "cleanup",
            "lease": {"page_id": JOB_PAGE, "holder": "h2"},
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let _ = TARGET;
}

#[tokio::test]
async fn a_malformed_lease_declaration_is_reported_and_promote_takes_no_lease() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);
    let bad = "---\nkind: skill\nid: broken\ndescription: x.\nvisibility: public\nlease:\n  holder: attempt\n---\n# broken\n";
    let v = call(
        &p,
        &t,
        "validate",
        json!({"as_page_id": "markdown/skills/broken.md", "content": bad}),
    )
    .await;
    assert!(
        v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "skill_rule_malformed"),
        "{v}"
    );
    // CONTROL: the well-formed declaration used above raises nothing.
    let v = call(
        &p,
        &t,
        "validate",
        json!({"as_page_id": "markdown/skills/job.md", "content": JOB_SKILL}),
    )
    .await;
    assert!(
        !v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "skill_rule_malformed"),
        "{v}"
    );
    // promote_changeset does not take the fence (it would be silently ignored).
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {t}"))
        .json(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"promote_changeset",
            "arguments":{"changeset_id":"x","lease":{"page_id":JOB_PAGE,"holder":"h1"}}}}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["result"]["structuredContent"]["issues"][0]["code"], "invalid_argument",
        "{body}"
    );
}
