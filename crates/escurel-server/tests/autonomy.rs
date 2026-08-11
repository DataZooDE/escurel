//! The `autonomy:` skill-page frontmatter key (heron#5 / CR-1), over real
//! HTTP against a running gateway with a real Indexer. No mocks, no LLM.
//!
//! `autonomy:` declares whether a write DERIVED from a skill may commit
//! directly (`auto`) or must be held for a human (`review` / `confirm`).
//! Escurel does not enforce the policy — it recognises the key so a typo is
//! caught where it is made.
//!
//! The load-bearing property is the direction of failure: an unrecognised
//! value must NEVER surface as `auto`. Getting that backwards turns
//! `autonmy: review` into an ungated write, which is the whole risk the key
//! exists to manage.

use escurel_test_support::{
    AuthMode, AutonomyLintMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role,
};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";

// A skill that declares nothing — the "must behave exactly as today" control.
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n---\n# note\n";
// The three recognised values, one per skill.
const PAYMENT_SKILL: &str =
    "---\ntype: skill\nid: payment\ndescription: Money moves.\nautonomy: confirm\n---\n# payment\n";
const TRIAGE_SKILL: &str =
    "---\ntype: skill\nid: triage\ndescription: Triage.\nautonomy: review\n---\n# triage\n";
// Deliberately padded + mixed case: the value is normalised, not matched raw.
const DRAFT_SKILL: &str =
    "---\ntype: skill\nid: draft\ndescription: Drafting.\nautonomy: \"  Auto  \"\n---\n# draft\n";
// The typo. Seeded while the write-time lint is Off — which is exactly the
// existing-tenant state the lint has to be rolled out over.
const BROKEN_SKILL: &str =
    "---\ntype: skill\nid: broken\ndescription: Fat fingers.\nautonomy: atuo\n---\n# broken\n";

fn catalogue() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", NOTE_SKILL)
        .skill("payment", PAYMENT_SKILL)
        .skill("triage", TRIAGE_SKILL)
        .skill("draft", DRAFT_SKILL)
        .skill("broken", BROKEN_SKILL)
        .done()
}

async fn start_with(lint: Option<AutonomyLintMode>, fixtures: FixtureBuilder) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures),
        config_overrides: ConfigOverrides {
            autonomy_lint: lint,
            ..Default::default()
        },
    })
    .await
}

/// Call a tool over MCP-over-HTTP, returning `structuredContent`.
async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "{name} error: {body}");
    body["result"]["structuredContent"].clone()
}

fn skill_page(id: &str, autonomy: Option<&str>) -> String {
    let line = autonomy.map_or_else(String::new, |a| format!("autonomy: {a}\n"));
    format!("---\ntype: skill\nid: {id}\ndescription: d.\n{line}---\n# {id}\n")
}

/// Does the issue list carry the unknown-autonomy finding?
fn has_autonomy_issue(v: &Value) -> bool {
    v["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .any(|i| i["code"] == "frontmatter_autonomy_unknown")
}

// --- list_skills ------------------------------------------------------

/// `list_skills` reports the DECLARED policy, and an unrecognised value is
/// reported as undeclared — never as `auto`.
#[tokio::test]
async fn list_skills_surfaces_autonomy_and_never_reads_unknown_as_auto() {
    let p = start_with(None, catalogue()).await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(&p, &token, "list_skills", json!({})).await;
    let by_id: std::collections::HashMap<&str, &Value> = out["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s))
        .collect();

    // Positive controls: every recognised value round-trips, including the
    // one that means "no gate". If these were absent the negatives below
    // would pass on a server that simply never emits the field.
    assert_eq!(by_id["payment"]["autonomy"], json!("confirm"));
    assert_eq!(by_id["triage"]["autonomy"], json!("review"));
    assert_eq!(by_id["draft"]["autonomy"], json!("auto"), "normalised");

    // Absence stays absence — a skill that declares nothing is not silently
    // given a policy it never asked for.
    assert_eq!(by_id["note"].get("autonomy"), None);

    // The typo. This is the assertion the whole key exists for.
    assert_ne!(
        by_id["broken"]["autonomy"],
        json!("auto"),
        "an unrecognised value must not read as `auto`"
    );
    assert_eq!(by_id["broken"].get("autonomy"), None);
}

/// The rest of the `list_skills` row is untouched for a skill that declares
/// no `autonomy:` — additive only.
#[tokio::test]
async fn a_skill_without_autonomy_keeps_its_whole_row() {
    let p = start_with(None, catalogue()).await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(&p, &token, "list_skills", json!({})).await;
    let note = out["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "note")
        .expect("note skill");
    assert_eq!(note["description"], json!("A note."));
    assert_eq!(note["visibility"], json!("public"));
    assert_eq!(note["layer"], json!("overlay"));
    assert_eq!(note["backend"]["kind"], json!("markdown"));
    assert_eq!(note.get("autonomy"), None);
}

/// `expand` is the RAW page, and stays raw: it reports what the author
/// wrote, junk included, so a curator fixing a flagged page sees the actual
/// bytes rather than a normalised guess. Recognising the key must not start
/// rewriting pages.
#[tokio::test]
async fn expand_reports_the_declared_autonomy_verbatim() {
    let p = start_with(None, catalogue()).await;
    let token = p.mint_token(TENANT, Role::Agent);

    // Positive control: a recognised value is passed through unchanged too —
    // `list_skills` normalises, `expand` does not.
    let out = call(
        &p,
        &token,
        "expand",
        json!({ "page_id": "markdown/skills/draft.md" }),
    )
    .await;
    assert_eq!(out["frontmatter"]["autonomy"], json!("  Auto  "));

    let out = call(&p, &token, "expand", json!({ "page_id": BROKEN_PAGE })).await;
    assert_eq!(out["frontmatter"]["autonomy"], json!("atuo"));
}

// --- validate ---------------------------------------------------------

/// `validate` errors on an unrecognised value and stays silent on each of
/// the three recognised ones (and on absence).
#[tokio::test]
async fn validate_errors_on_unknown_autonomy_only() {
    let p = start_with(None, catalogue()).await;
    let token = p.mint_token(TENANT, Role::Agent);

    // Positive controls first: the recognised set, plus absence.
    for good in ["auto", "review", "confirm", "AUTO", "\"  confirm \""] {
        let out = call(
            &p,
            &token,
            "validate",
            json!({ "content": skill_page("note", Some(good)) }),
        )
        .await;
        assert!(!has_autonomy_issue(&out), "`{good}` should be accepted");
        assert_eq!(out["ok"], json!(true), "`{good}`: {out}");
    }
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill_page("note", None) }),
    )
    .await;
    assert!(!has_autonomy_issue(&out), "absence is not a finding");
    assert_eq!(out["ok"], json!(true));

    // The negatives.
    for bad in ["atuo", "yes", "\"\"", "true", "[auto]"] {
        let out = call(
            &p,
            &token,
            "validate",
            json!({ "content": skill_page("note", Some(bad)) }),
        )
        .await;
        assert!(has_autonomy_issue(&out), "`{bad}` should be flagged: {out}");
        assert_eq!(out["ok"], json!(false), "`{bad}` must fail validation");
        let issue = out["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["code"] == "frontmatter_autonomy_unknown")
            .unwrap();
        assert_eq!(issue["severity"], json!("error"));
        assert_eq!(issue["location"], json!("frontmatter.autonomy"));
        assert!(
            issue["suggestion"]
                .as_str()
                .unwrap_or_default()
                .contains("confirm"),
            "the suggestion names the recognised set: {issue}"
        );
    }
}

/// The key is a SKILL-page key. On an instance page `autonomy:` is still
/// free-form frontmatter and must not start failing.
#[tokio::test]
async fn validate_ignores_autonomy_on_an_instance_page() {
    let p = start_with(None, catalogue()).await;
    let token = p.mint_token(TENANT, Role::Agent);

    let instance =
        "---\ntype: instance\nskill: note\nid: n1\nautonomy: atuo\n---\n# n1\n".to_owned();
    let out = call(&p, &token, "validate", json!({ "content": instance })).await;
    assert!(
        !has_autonomy_issue(&out),
        "instance pages are out of scope: {out}"
    );
    assert_eq!(out["ok"], json!(true));

    // Positive control in the same test: the identical value on a SKILL
    // page is flagged, so the silence above is scoping and not a dead check.
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill_page("note", Some("atuo")) }),
    )
    .await;
    assert!(has_autonomy_issue(&out), "{out}");
}

// --- update_page, and the rollout gate --------------------------------

const BROKEN_PAGE: &str = "markdown/skills/broken.md";

/// Rewriting a skill page with a dangling wikilink. Used as the positive
/// control that the write-time validation gate is live in every mode.
fn dangling_skill(id: &str, autonomy: Option<&str>) -> String {
    let line = autonomy.map_or_else(String::new, |a| format!("autonomy: {a}\n"));
    format!(
        "---\ntype: skill\nid: {id}\ndescription: d.\n{line}---\n# {id}\nSee [[nosuchskill::x]].\n"
    )
}

/// Default (`Off`): an unrecognised value is reported by `validate` but does
/// not block the write. This is the compatibility contract for a tenant that
/// already has junk in the field.
#[tokio::test]
async fn update_page_allows_unknown_autonomy_when_the_lint_is_off() {
    let p = start_with(None, catalogue()).await;
    let token = p.mint_token(TENANT, Role::Admin);

    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": skill_page("broken", Some("stil-atuo")) }),
    )
    .await;
    assert_eq!(out["ok"], json!(true), "off must not block: {out}");

    // Positive control: the pre-existing write gate still refuses what it
    // always refused, so `ok: true` above is permission and not a dead gate.
    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": dangling_skill("broken", Some("atuo")) }),
    )
    .await;
    assert_eq!(out["ok"], json!(false), "dangling link still blocks: {out}");
}

/// `Log`: observed, warned about, still written — the middle rung of the
/// dark → log → enforce rollout.
#[tokio::test]
async fn update_page_allows_unknown_autonomy_in_log_mode() {
    let p = start_with(Some(AutonomyLintMode::Log), catalogue()).await;
    let token = p.mint_token(TENANT, Role::Admin);

    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": skill_page("broken", Some("atuo")) }),
    )
    .await;
    // Log mode differs from Enforce only in the verdict: the write lands, and
    // the response is byte-for-byte the one an unlinted server sends, so a
    // tenant can be observed without any client noticing.
    assert_eq!(out["ok"], json!(true), "log mode must not block: {out}");
    assert_eq!(out["issues"], json!([]), "{out}");

    // Positive control: log mode does not disable the rest of the gate.
    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": dangling_skill("broken", None) }),
    )
    .await;
    assert_eq!(out["ok"], json!(false), "{out}");
}

/// `Enforce`: the mistake is refused where it is made.
#[tokio::test]
async fn update_page_rejects_unknown_autonomy_under_enforce() {
    let p = start_with(Some(AutonomyLintMode::Enforce), catalogue()).await;
    let token = p.mint_token(TENANT, Role::Admin);

    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": skill_page("broken", Some("atuo")) }),
    )
    .await;
    assert_eq!(out["ok"], json!(false), "enforce must block: {out}");
    assert!(has_autonomy_issue(&out), "{out}");

    // Positive control: a recognised value writes fine under enforce, so the
    // rejection above is the value and not the mode refusing everything.
    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": skill_page("broken", Some("review")) }),
    )
    .await;
    assert_eq!(out["ok"], json!(true), "{out}");

    // And an instance page carrying the same junk still writes: enforce is
    // scoped to skill pages exactly as `validate` is.
    let out = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": "markdown/instances/note/n1.md",
            "content": "---\ntype: instance\nskill: note\nid: n1\nautonomy: atuo\n---\n# n1\n",
        }),
    )
    .await;
    assert_eq!(out["ok"], json!(true), "{out}");
}

/// Enforce also refuses a page whose junk was already there: the write is
/// where the correction happens, so there is no way to re-save the mistake.
#[tokio::test]
async fn enforce_blocks_an_unrelated_edit_to_a_page_that_already_has_junk() {
    let p = start_with(Some(AutonomyLintMode::Enforce), catalogue()).await;
    let token = p.mint_token(TENANT, Role::Admin);

    // Body edit only; the junk `autonomy:` is carried over untouched.
    let body_edit = "---\ntype: skill\nid: broken\ndescription: d.\nautonomy: atuo\n---\n# broken\nMore text.\n";
    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": body_edit }),
    )
    .await;
    assert_eq!(out["ok"], json!(false), "{out}");

    // Positive control: the same body edit with the value corrected lands.
    let fixed = "---\ntype: skill\nid: broken\ndescription: d.\nautonomy: auto\n---\n# broken\nMore text.\n";
    let out = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": BROKEN_PAGE, "content": fixed }),
    )
    .await;
    assert_eq!(out["ok"], json!(true), "{out}");
}
