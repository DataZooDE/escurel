//! A skill page may keep its instances out of a default search
//! (`search: {hidden: true, exact: [...]}`): free text no longer reaches them,
//! an exact id or declared `exact` field value still does, the per-instance
//! read ACL still applies, and `skill:` searches them as before. Real gateway,
//! real HTTP.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

/// Many small rows nobody should meet in a free-text search: owner-private,
/// found by their `code` exactly.
const NOTE_ROW_SKILL: &str = "---\nkind: skill\nid: note-row\n\
    description: Ledger of short widget notes.\nvisibility: owner\nowner_field: owner\n\
    search:\n  hidden: true\n  exact: [code]\n\
    fields:\n  - {name: code, kind: string}\n  - {name: title, kind: string}\n  - {name: owner, kind: string}\n\
    ---\n# note-row\nLedger of short widget notes, one row per note.\n";
const MEMO_SKILL: &str = "---\nkind: skill\nid: memo\ndescription: A memo.\n---\n# memo\n";

const ALPHA: &str = "---\nkind: instance\nskill: note-row\nid: nr-alpha\ncode: NR-0042\n\
    title: Alpha review\nowner: \"whatsapp:111\"\n---\n# nr-alpha\n\
    Widget widget widget: the widget gearbox review for the widget line.\n";
const BETA: &str = "---\nkind: instance\nskill: note-row\nid: nr-beta\ncode: NR-0099\n\
    title: Beta review\nowner: \"whatsapp:222\"\n---\n# nr-beta\n\
    Widget widget widget: the widget housing review for the widget line.\n";
/// Two owners' rows share a code; the other owner's sorts first by page id.
const DUP_BOB: &str = "---\nkind: instance\nskill: note-row\nid: nr-aaa\ncode: NR-DUP\n\
    owner: \"whatsapp:222\"\n---\n# nr-aaa\nA shared code.\n";
const DUP_ALICE: &str = "---\nkind: instance\nskill: note-row\nid: nr-zzz\ncode: NR-DUP\n\
    owner: \"whatsapp:111\"\n---\n# nr-zzz\nA shared code.\n";
/// A row that exists only in scenario `b`.
const GAMMA_B: &str = "---\nkind: instance\nskill: note-row\nid: nr-gamma\ncode: NR-0777\n\
    scenario: b\nowner: \"whatsapp:111\"\n---\n# nr-gamma\nA what-if row.\n";
const PLAN: &str = "---\nkind: instance\nskill: memo\nid: widget-plan\n---\n# widget-plan\n\
    The plan for the widget line next year.\n";

const ALPHA_PAGE: &str = "markdown/instances/note-row/nr-alpha.md";
const BETA_PAGE: &str = "markdown/instances/note-row/nr-beta.md";
const PLAN_PAGE: &str = "markdown/instances/memo/widget-plan.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note-row", NOTE_ROW_SKILL)
                .skill("memo", MEMO_SKILL)
                .instance("note-row", "nr-alpha", ALPHA)
                .instance("note-row", "nr-beta", BETA)
                .instance("note-row", "nr-aaa", DUP_BOB)
                .instance("note-row", "nr-zzz", DUP_ALICE)
                .instance("note-row", "nr-gamma", GAMMA_B)
                .instance("memo", "widget-plan", PLAN)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let v: Value = reqwest::Client::new()
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
    assert!(v.get("error").is_none(), "{name} error: {v}");
    v["result"]["structuredContent"].clone()
}

fn page_ids(v: &Value) -> Vec<String> {
    v["hits"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| h["page_id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn codes(v: &Value) -> Vec<String> {
    v["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

async fn search(p: &EscurelProcess, token: &str, args: Value) -> Vec<String> {
    page_ids(&rpc(p, token, "search", args).await)
}

/// Free text does not reach a hidden skill's instances; the top k is filled
/// with other content. Naming the skill brings them back.
#[tokio::test]
async fn hidden_instances_stay_out_of_default_search() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let hits = search(&p, &admin, json!({ "q": "widget review", "k": 10 })).await;
    assert!(
        !hits.iter().any(|h| h == ALPHA_PAGE || h == BETA_PAGE),
        "hidden instances leaked into a default search: {hits:?}"
    );
    assert!(hits.iter().any(|h| h == PLAN_PAGE), "{hits:?}");

    // `skill:` searches the hidden skill exactly as before.
    let scoped = search(
        &p,
        &admin,
        json!({ "q": "widget review", "k": 10, "skill": "note-row" }),
    )
    .await;
    assert!(
        scoped.iter().any(|h| h == ALPHA_PAGE) && scoped.iter().any(|h| h == BETA_PAGE),
        "skill: note-row finds its instances: {scoped:?}"
    );
}

/// The skill page is catalogue, not an instance: it stays searchable.
#[tokio::test]
async fn the_hidden_skills_own_page_is_still_found() {
    let p = start().await;
    let agent = p.mint_token(TENANT, Role::Agent);
    for args in [
        json!({ "q": "ledger of short widget notes", "k": 10, "page_kind": "skill" }),
        json!({ "q": "ledger of short widget notes", "k": 10 }),
    ] {
        let hits = search(&p, &agent, args.clone()).await;
        assert!(
            hits.iter().any(|h| h == "markdown/skills/note-row.md"),
            "{args}: {hits:?}"
        );
    }
}

/// An exact page id, slug / `id`, or declared `exact` field value reveals a
/// hidden instance in a default search, case-insensitively, ranked first. A
/// substring or another field's value does not.
#[tokio::test]
async fn an_exact_lookup_finds_a_hidden_instance() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    for q in [
        "NR-0042",
        "nr-0042",
        " nr-0042 ",
        "NR-ALPHA",
        "nr-alpha",
        ALPHA_PAGE,
    ] {
        let hits = search(&p, &alice, json!({ "q": q, "k": 5 })).await;
        assert_eq!(
            hits.first().map(String::as_str),
            Some(ALPHA_PAGE),
            "{q:?} finds nr-alpha first: {hits:?}"
        );
    }
    // One of several query variants is enough.
    let hits = search(
        &p,
        &alice,
        json!({ "queries": ["gearbox notes", "NR-0042"], "k": 5 }),
    )
    .await;
    assert_eq!(
        hits.first().map(String::as_str),
        Some(ALPHA_PAGE),
        "{hits:?}"
    );

    for q in ["NR-004", "0042", "alpha review", "Alpha", "nr"] {
        let hits = search(&p, &alice, json!({ "q": q, "k": 10 })).await;
        assert!(
            !hits.iter().any(|h| h == ALPHA_PAGE),
            "{q:?} is not an exact match but found nr-alpha: {hits:?}"
        );
    }
    // The search's own constraints still apply to an exact hit.
    let skills_only = search(
        &p,
        &alice,
        json!({ "q": "NR-0042", "k": 5, "page_kind": "skill" }),
    )
    .await;
    assert!(
        !skills_only.iter().any(|h| h == ALPHA_PAGE),
        "{skills_only:?}"
    );
    let filtered = search(
        &p,
        &alice,
        json!({ "q": "NR-0042", "k": 5, "filter": { "title": "Beta review" } }),
    )
    .await;
    assert!(!filtered.iter().any(|h| h == ALPHA_PAGE), "{filtered:?}");
    let past_cut = search(
        &p,
        &alice,
        json!({ "q": "NR-0042", "k": 5, "as_of": "2000-01-01T00:00:00Z" }),
    )
    .await;
    // Untimed pages stay visible under `as_of`, like the native lane.
    assert_eq!(
        past_cut.first().map(String::as_str),
        Some(ALPHA_PAGE),
        "{past_cut:?}"
    );
}

/// The read ACL applies to an exact hit: another owner's private instance is
/// not returned, whatever the query names.
#[tokio::test]
async fn an_exact_lookup_respects_the_read_acl() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    for q in ["NR-0099", "nr-beta", BETA_PAGE] {
        let hits = search(&p, &alice, json!({ "q": q, "k": 5 })).await;
        assert!(
            !hits.iter().any(|h| h == BETA_PAGE),
            "alice must not see bob's row via {q:?}: {hits:?}"
        );
        let hits = search(&p, &bob, json!({ "q": q, "k": 5 })).await;
        assert_eq!(hits.first().map(String::as_str), Some(BETA_PAGE), "{q:?}");
    }
}

/// `list_skills` publishes the setting.
#[tokio::test]
async fn list_skills_publishes_the_search_setting() {
    let p = start().await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let r = rpc(&p, &agent, "list_skills", json!({})).await;
    let skills = r["skills"].as_array().cloned().unwrap_or_default();
    let row = |id: &str| {
        skills
            .iter()
            .find(|s| s["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} listed: {r}"))
    };
    let note = row("note-row");
    assert_eq!(note["search_hidden"], true, "{note}");
    assert_eq!(note["search_exact"], json!(["code"]), "{note}");
    let memo = row("memo");
    assert!(memo.get("search_hidden").is_none(), "{memo}");
    assert!(memo.get("search_exact").is_none(), "{memo}");
}

/// A malformed `search:` block is refused on write with `search_invalid`; an
/// `exact` entry that is not a declared field is a warning only.
#[tokio::test]
async fn a_malformed_search_block_is_refused() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let page = |block: &str| {
        format!(
            "---\nkind: skill\nid: probe\ndescription: Probe.\n\
             fields:\n  - {{name: code, kind: string}}\n{block}---\n# probe\n"
        )
    };
    for block in [
        "search: true\n",
        "search: [hidden]\n",
        "search:\n  hidden: \"yes\"\n",
        "search:\n  exact: code\n",
        "search:\n  exact: [1]\n",
        "search:\n  exact: [\"a b\"]\n",
        "search:\n  hidden: true\n  rank: 3\n",
    ] {
        let content = page(block);
        let v = rpc(&p, &admin, "validate", json!({ "content": content })).await;
        assert!(
            codes(&v).contains(&"search_invalid".to_owned()),
            "validate {block:?}: {v}"
        );
        let w = rpc(
            &p,
            &admin,
            "update_page",
            json!({ "page_id": "markdown/skills/probe.md", "content": content }),
        )
        .await;
        assert_eq!(w["ok"], false, "update_page stored {block:?}: {w}");
        assert!(codes(&w).contains(&"search_invalid".to_owned()), "{w}");
    }
    // An undeclared `exact` field is a warning: `validate` names it, the page is stored.
    let content = page("search:\n  hidden: true\n  exact: [code, serial]\n");
    let v = rpc(&p, &admin, "validate", json!({ "content": content })).await;
    assert!(
        codes(&v).contains(&"search_exact_unknown".to_owned())
            && !codes(&v).contains(&"search_invalid".to_owned()),
        "the undeclared field is named: {v}"
    );
    let w = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/probe.md", "content": content }),
    )
    .await;
    assert_eq!(w["ok"], true, "{w}");
    // The well-formed minimal block is fine.
    let w = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/probe.md",
                "content": page("search: {hidden: false}\n") }),
    )
    .await;
    assert_eq!(w["ok"], true, "{w}");
    assert!(codes(&w).is_empty(), "{w}");
}

/// The ACL is applied before the exact hits are cut to `k`: another owner's row
/// with the same value, sorting first, does not push the caller's own out.
#[tokio::test]
async fn an_exact_lookup_keeps_the_callers_row_when_another_owner_shares_the_value() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let hits = search(&p, &alice, json!({ "q": "NR-DUP", "k": 1 })).await;
    assert_eq!(
        hits,
        vec!["markdown/instances/note-row/nr-zzz.md".to_owned()],
        "alice's own row survives k: 1"
    );
}

/// An exact hit honours the scenario like the native lane: an overlay-only row
/// is absent from the base search and present in its scenario.
#[tokio::test]
async fn an_exact_lookup_honours_the_scenario() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let gamma = "markdown/instances/note-row/nr-gamma.md";
    let base = search(&p, &alice, json!({ "q": "NR-0777", "k": 5 })).await;
    assert!(!base.iter().any(|h| h == gamma), "{base:?}");
    let in_b = search(
        &p,
        &alice,
        json!({ "q": "NR-0777", "k": 5, "scenario": "b" }),
    )
    .await;
    assert_eq!(in_b.first().map(String::as_str), Some(gamma), "{in_b:?}");
}
