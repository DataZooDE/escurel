//! Schema-ergonomics contract over real HTTP (2026-08-14 API review,
//! consumer-ergonomics findings F1/F4 + naming B5/B9).
//!
//! Three classes of trap this pins shut:
//! - `capture_event` accepted `{}` and minted an unroutable junk event
//!   (empty `label_skill` = nothing for the runner to route on);
//! - sibling tools spelled the same concept differently (`skill` on
//!   `search` vs `skill_id` on `list_instances`; `from`/`to` vs
//!   `from_page_id`/`to_page_id`) and unknown args are silently
//!   dropped, so the WRONG spelling "succeeded" with default behaviour;
//! - a handful of admin write envelopes omitted `ok`, so their
//!   refusals could never reach MCP `isError`.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const NOTE_A: &str = "---\ntype: instance\nskill: note\nid: a\n---\n# A\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "a", NOTE_A)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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
        .expect("decode")
}

/// An event with no `label_skill` is unroutable — the runner selects
/// its system prompt by that label. `{}` used to succeed silently.
#[tokio::test]
async fn capture_event_requires_a_label_skill() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let refused = call(&p, &token, "capture_event", json!({ "source": "t" })).await;
    assert_eq!(
        refused["error"]["code"],
        json!(-32602),
        "an unlabelled capture must be refused, not minted as junk: {refused}"
    );

    let ok = call(
        &p,
        &token,
        "capture_event",
        json!({ "source": "t", "label_skill": "note" }),
    )
    .await;
    assert!(ok.get("error").is_none(), "a labelled capture lands: {ok}");
}

/// `search` filters by `skill`, `list_instances` by `skill_id` — real
/// wire divergence, and the wrong spelling was silently dropped. Each
/// now accepts the sibling's spelling as an alias.
#[tokio::test]
async fn skill_id_and_skill_alias_each_other() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // list_instances with the `skill` spelling (search's) must work.
    let li = call(&p, &token, "list_instances", json!({ "skill": "note" })).await;
    assert!(li.get("error").is_none(), "alias `skill` accepted: {li}");
    let instances = li["result"]["structuredContent"]["instances"]
        .as_array()
        .unwrap_or_else(|| panic!("instances shape: {li}"));
    assert_eq!(instances.len(), 1, "the note instance is found: {li}");

    // search with the `skill_id` spelling (list_instances') must not
    // error (hits may be empty — ZeroEmbedder — but the arg must bind).
    let se = call(
        &p,
        &token,
        "search",
        json!({ "q": "a", "skill_id": "note" }),
    )
    .await;
    assert!(se.get("error").is_none(), "alias `skill_id` accepted: {se}");
}

/// `move_page` spelled its pair `from`/`to` while `provenance_path`
/// says `from_page`/`to_page` — the long spellings now alias.
#[tokio::test]
async fn move_page_accepts_the_long_page_pair_spelling() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let moved = call(
        &p,
        &token,
        "move_page",
        json!({
            "from_page_id": "markdown/instances/note/a.md",
            "to_page_id": "markdown/instances/note/b.md",
        }),
    )
    .await;
    assert!(
        moved.get("error").is_none(),
        "long spellings must bind, not vanish into the unknown-arg void: {moved}"
    );
    assert_eq!(
        moved["result"]["structuredContent"]["ok"],
        json!(true),
        "the move lands: {moved}"
    );
}

/// `admin_delete_chat_history` returned bare `{deleted}` — an envelope
/// that can never carry `ok:false`, so MCP `isError` was unreachable
/// for it. Success now says `ok: true` like its admin siblings.
#[tokio::test]
async fn admin_chat_purge_envelope_carries_ok() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let out = call(
        &p,
        &admin,
        "admin_delete_chat_history",
        json!({ "chat_group_id": "nobody" }),
    )
    .await;
    let s = &out["result"]["structuredContent"];
    assert_eq!(s["ok"], json!(true), "envelope carries ok: {out}");
    assert!(
        s["deleted"].is_number(),
        "payload unchanged beside it: {out}"
    );
}
