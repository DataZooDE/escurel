//! Separation of duties by person: the `same_person` claim.
//!
//! One human can reach a tenant under several subjects (one per identity
//! provider). An issuer that knows this names the other subjects in the token
//! (`same_person`), and the gateway then decides review by person, not by
//! subject:
//!
//! - the caller may not promote a draft (or a changeset with a draft) that any
//!   of its subjects wrote — `self_review`, nothing lands;
//! - the caller may discard its own drafts under any of those subjects
//!   (withdraw), and another person's draft only when it may make the write
//!   the draft proposes (a reviewer) or is an admin — else `not_author`;
//! - a token without the claim behaves exactly as before (the issuer asked for
//!   nothing), and a machine token never carries a person;
//! - a malformed claim is an invalid token, never a shorter list.
//!
//! Real gateway, real DuckDB, real HTTP. No mocks.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const GOOGLE: &str = "google:jr";
const ENTRA: &str = "entra:t:jr";
const EVE: &str = "entra:t:eve";
const READER: &str = "entra:t:reader";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: A note.\n\
    acl:\n  read: [public]\n  create: [editors]\n  update: [editors]\n  propose: [proposers]\n---\n# note\n";

fn page(id: &str) -> String {
    format!("markdown/instances/note/{id}.md")
}

fn body(id: &str) -> String {
    format!("---\nkind: instance\nskill: note\nid: {id}\n---\n# {id}\n")
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .done(),
        ),
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let (_, body) = rpc(p, token, tool, args).await;
    assert!(body.get("error").is_none(), "{tool} error: {body}");
    body["result"]["structuredContent"].clone()
}

/// An editor's token for `subject`, naming `same_person` when given.
fn person(p: &EscurelProcess, subject: &str, same_person: Option<&[&str]>) -> String {
    let token = p.mint_token_with_groups(TENANT, subject, &["editors"], false);
    match same_person {
        Some(others) => p.resign_token_with_claim(&token, "same_person", json!(others)),
        None => token,
    }
}

async fn draft(p: &EscurelProcess, token: &str, id: &str) -> String {
    let made = call(
        p,
        token,
        "create_draft",
        json!({ "target_page_id": page(id), "content": body(id) }),
    )
    .await;
    assert_eq!(made["ok"], json!(true), "{made}");
    made["draft"]["draft_id"]
        .as_str()
        .expect("draft_id")
        .to_owned()
}

async fn status(p: &EscurelProcess, id: &str) -> Value {
    let admin = p.mint_token(TENANT, Role::Admin);
    call(p, &admin, "get_draft", json!({ "draft_id": id })).await["draft"]["status"].clone()
}

#[tokio::test]
async fn a_person_does_not_accept_a_draft_any_of_their_subjects_wrote() {
    let p = start().await;
    let via_entra = person(&p, ENTRA, Some(&[GOOGLE]));
    let via_google = person(&p, GOOGLE, Some(&[ENTRA]));

    // whoami says who the issuer takes the caller for.
    let me = call(&p, &via_google, "whoami", json!({})).await;
    assert_eq!(me["same_person"], json!([ENTRA]), "{me}");

    let filed = draft(&p, &via_entra, "from-claude").await;
    let refused = call(
        &p,
        &via_google,
        "promote_draft",
        json!({ "draft_id": filed }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert_eq!(
        refused["issues"][0]["code"],
        json!("self_review"),
        "{refused}"
    );
    assert_eq!(status(&p, &filed).await, json!("open"), "nothing landed");
    let admin = p.mint_token(TENANT, Role::Admin);
    let landed = call(
        &p,
        &admin,
        "expand",
        json!({ "page_id": page("from-claude") }),
    )
    .await;
    assert!(
        landed["page"].is_null() || landed["content"].is_null(),
        "no page: {landed}"
    );
    // An admin vouching for the author (`decided_by`) under an asserted person is refused too.
    let vouching = p.resign_token_with_claim(&admin, "same_person", json!([]));
    let vouched = call(
        &p,
        &vouching,
        "promote_draft",
        json!({ "draft_id": filed, "decided_by": ENTRA }),
    )
    .await;
    assert_eq!(
        vouched["issues"][0]["code"],
        json!("self_review"),
        "{vouched}"
    );
    // The same subject too, once the issuer asserts a person (even with no other subject).
    let alone = person(&p, ENTRA, Some(&[]));
    let own = call(&p, &alone, "promote_draft", json!({ "draft_id": filed })).await;
    assert_eq!(own["issues"][0]["code"], json!("self_review"), "{own}");

    // A changeset with one of the person's drafts is refused as a whole.
    let cs = call(
        &p,
        &via_entra,
        "create_draft",
        json!({ "target_page_id": page("cs-a"), "content": body("cs-a"), "new_changeset": true }),
    )
    .await;
    let changeset = cs["draft"]["changeset_id"]
        .as_str()
        .expect("changeset")
        .to_owned();
    let batch = call(
        &p,
        &via_google,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(batch["issues"][0]["code"], json!("self_review"), "{batch}");

    // Another person accepts it.
    let eve = person(&p, EVE, Some(&[]));
    let accepted = call(&p, &eve, "promote_draft", json!({ "draft_id": filed })).await;
    assert_eq!(accepted["ok"], json!(true), "{accepted}");
    assert_eq!(status(&p, &filed).await, json!("promoted"));
}

#[tokio::test]
async fn a_person_withdraws_their_own_drafts_and_only_a_reviewer_rejects_another_persons() {
    let p = start().await;
    let via_entra = person(&p, ENTRA, Some(&[GOOGLE]));
    let via_google = person(&p, GOOGLE, Some(&[ENTRA]));
    let filed = draft(&p, &via_entra, "to-withdraw").await;
    let withdrawn = call(
        &p,
        &via_google,
        "discard_draft",
        json!({ "draft_id": filed, "reason": "withdrawn by its author" }),
    )
    .await;
    assert_eq!(withdrawn["ok"], json!(true), "{withdrawn}");
    assert_eq!(status(&p, &filed).await, json!("discarded"));

    // A person who may read the note but not write it does not reject another person's draft.
    let other = draft(&p, &via_entra, "to-reject").await;
    let reader = p.mint_token_with_groups(TENANT, READER, &[], false);
    let reader = p.resign_token_with_claim(&reader, "same_person", json!([]));
    let denied = call(
        &p,
        &reader,
        "discard_draft",
        json!({ "draft_id": other, "reason": "no" }),
    )
    .await;
    assert_eq!(denied["issues"][0]["code"], json!("not_author"), "{denied}");
    assert_eq!(status(&p, &other).await, json!("open"));
    // A reviewer (an editor: may make the write) rejects it.
    let eve = person(&p, EVE, Some(&[]));
    let rejected = call(
        &p,
        &eve,
        "discard_draft",
        json!({ "draft_id": other, "reason": "no" }),
    )
    .await;
    assert_eq!(rejected["ok"], json!(true), "{rejected}");

    // Without the claim the reader may discard as before: the issuer asked for nothing.
    let legacy = draft(&p, &via_entra, "legacy").await;
    let plain_reader = p.mint_token_with_groups(TENANT, READER, &[], false);
    let before = call(
        &p,
        &plain_reader,
        "discard_draft",
        json!({ "draft_id": legacy, "reason": "no" }),
    )
    .await;
    assert_eq!(before["ok"], json!(true), "{before}");
}

#[tokio::test]
async fn without_the_claim_the_subject_is_still_separated_and_a_bad_claim_is_an_invalid_token() {
    let p = start().await;
    let plain = person(&p, GOOGLE, None);
    let filed = draft(&p, &plain, "own").await;
    let me = call(&p, &plain, "whoami", json!({})).await;
    assert!(me.get("same_person").is_none(), "{me}");
    // Nobody accepts their own proposal, also without the claim (the subject itself is the person).
    let own = call(&p, &plain, "promote_draft", json!({ "draft_id": filed })).await;
    assert_eq!(own["issues"][0]["code"], json!("self_review"), "{own}");
    // No admin break-glass: an admin's own draft waits for another person too.
    let admin = p.mint_token(TENANT, Role::Admin);
    let admins = draft(&p, &admin, "admins-own").await;
    let refused = call(&p, &admin, "promote_draft", json!({ "draft_id": admins })).await;
    assert_eq!(
        refused["issues"][0]["code"],
        json!("self_review"),
        "{refused}"
    );
    // A service that filed a draft may record the human who decided it, never its author.
    let vouched_author = call(
        &p,
        &admin,
        "promote_draft",
        json!({ "draft_id": filed, "decided_by": GOOGLE }),
    )
    .await;
    assert_eq!(
        vouched_author["issues"][0]["code"],
        json!("self_review"),
        "{vouched_author}"
    );
    // Naming someone else is no way round it either: the admin wrote the draft.
    let vouched = call(
        &p,
        &admin,
        "promote_draft",
        json!({ "draft_id": admins, "decided_by": EVE }),
    )
    .await;
    assert_eq!(
        vouched["issues"][0]["code"],
        json!("self_review"),
        "{vouched}"
    );
    // Another admin records the human who decided (the admin's draft, decided by eve).
    let other_admin = p.mint_token_with_sub(TENANT, Role::Admin, "service:recorder");
    let recorded = call(
        &p,
        &other_admin,
        "promote_draft",
        json!({ "draft_id": admins, "decided_by": EVE }),
    )
    .await;
    assert_eq!(recorded["ok"], json!(true), "{recorded}");
    assert_eq!(recorded["decided_by"], json!(EVE), "{recorded}");
    // Another person accepts the first one.
    let promoted = call(
        &p,
        &person(&p, EVE, None),
        "promote_draft",
        json!({ "draft_id": filed }),
    )
    .await;
    assert_eq!(promoted["ok"], json!(true), "{promoted}");

    for bad in [
        json!("google:jr"),
        json!(null),
        json!([1]),
        json!([""]),
        json!(["a\u{7}b"]),
        json!(["x".repeat(257)]),
        json!(vec!["s"; 33]),
    ] {
        let token =
            p.resign_token_with_claim(&person(&p, GOOGLE, None), "same_person", bad.clone());
        let (status, body) = rpc(&p, &token, "whoami", json!({})).await;
        assert_eq!(status, 401, "{bad}: {body}");
    }

    // A machine token carries no person, whatever it claims.
    let acting = p.mint_token_acting_as(TENANT, GOOGLE, "agent:helper");
    let acting = p.resign_token_with_claim(&acting, "same_person", json!([ENTRA]));
    let machine = call(&p, &acting, "whoami", json!({})).await;
    assert!(machine.get("same_person").is_none(), "{machine}");
}

#[tokio::test]
async fn a_changeset_is_rejected_by_a_reviewer_and_a_stale_draft_is_superseded_only_with_that_right()
 {
    let p = start().await;
    let via_entra = person(&p, ENTRA, Some(&[GOOGLE]));
    let cs = call(
        &p,
        &via_entra,
        "create_draft",
        json!({ "target_page_id": page("cs-b"), "content": body("cs-b"), "new_changeset": true }),
    )
    .await;
    let changeset = cs["draft"]["changeset_id"]
        .as_str()
        .expect("changeset")
        .to_owned();
    let reader = p.resign_token_with_claim(
        &p.mint_token_with_groups(TENANT, READER, &[], false),
        "same_person",
        json!([]),
    );
    let denied = call(
        &p,
        &reader,
        "discard_changeset",
        json!({ "changeset_id": changeset, "reason": "no" }),
    )
    .await;
    assert_eq!(denied["issues"][0]["code"], json!("not_author"), "{denied}");
    let admin =
        p.resign_token_with_claim(&p.mint_token(TENANT, Role::Admin), "same_person", json!([]));
    let rejected = call(
        &p,
        &admin,
        "discard_changeset",
        json!({ "changeset_id": changeset, "reason": "no" }),
    )
    .await;
    assert_eq!(rejected["ok"], json!(true), "an admin rejects: {rejected}");

    // A stale draft of another person: a proposer (no write right) does not supersede it.
    let editor = p.mint_token(TENANT, Role::Admin);
    call(
        &p,
        &editor,
        "update_page",
        json!({ "page_id": page("stale"), "content": body("stale") }),
    )
    .await;
    let base =
        call(&p, &editor, "expand", json!({ "page_id": page("stale") })).await["content_sha256"]
            .clone();
    let eve = person(&p, EVE, Some(&[]));
    let stale = call(
        &p,
        &eve,
        "create_draft",
        json!({ "target_page_id": page("stale"), "content": format!("{}Eve.\n", body("stale")), "base_sha256": base }),
    )
    .await;
    let stale_id = stale["draft"]["draft_id"]
        .as_str()
        .expect("draft")
        .to_owned();
    call(
        &p,
        &editor,
        "update_page",
        json!({ "page_id": page("stale"), "content": format!("{}Moved.\n", body("stale")) }),
    )
    .await;
    let head =
        call(&p, &editor, "expand", json!({ "page_id": page("stale") })).await["content_sha256"]
            .clone();
    let proposer = p.resign_token_with_claim(
        &p.mint_token_with_groups(TENANT, "entra:t:proposer", &["proposers"], false),
        "same_person",
        json!([]),
    );
    let blocked = call(
        &p,
        &proposer,
        "create_draft",
        json!({ "target_page_id": page("stale"), "content": format!("{}Mine.\n", body("stale")), "base_sha256": head }),
    )
    .await;
    assert_eq!(blocked["issues"][0]["code"], json!("conflict"), "{blocked}");
    assert_eq!(
        status(&p, &stale_id).await,
        json!("open"),
        "the predecessor stays open"
    );
    // An editor (who may make the write) supersedes it as before.
    let superseding = person(&p, GOOGLE, Some(&[]));
    let replaced = call(
        &p,
        &superseding,
        "create_draft",
        json!({ "target_page_id": page("stale"), "content": format!("{}Mine.\n", body("stale")), "base_sha256": head }),
    )
    .await;
    assert_eq!(replaced["ok"], json!(true), "{replaced}");
    assert_eq!(status(&p, &stale_id).await, json!("discarded"));
}

#[tokio::test]
async fn a_person_does_not_merge_their_own_branch_another_person_does() {
    let p = start().await;
    let jr = person(&p, GOOGLE, Some(&[ENTRA]));
    let made = call(&p, &jr, "create_branch", json!({ "name": "jr/work" })).await;
    assert_eq!(made["ok"], json!(true), "{made}");
    let wrote = call(
        &p,
        &jr,
        "update_page",
        json!({ "page_id": page("on-branch"), "content": body("on-branch"), "branch": "jr/work" }),
    )
    .await;
    assert_eq!(wrote["ok"], json!(true), "{wrote}");
    let own = call(&p, &jr, "merge_branch", json!({ "name": "jr/work" })).await;
    assert_eq!(own["issues"][0]["code"], json!("self_review"), "{own}");
    // A branch is its author's workspace: the Entra subject does not see it.
    let entra = person(&p, ENTRA, Some(&[GOOGLE]));
    let other = call(&p, &entra, "merge_branch", json!({ "name": "jr/work" })).await;
    assert_eq!(
        other["issues"][0]["code"],
        json!("unknown_branch"),
        "{other}"
    );
    // An admin who is the same person (its claim names the author) does not merge it either.
    let same_admin = p.resign_token_with_claim(
        &p.mint_token_with_sub(TENANT, Role::Admin, ENTRA),
        "same_person",
        json!([GOOGLE]),
    );
    let refused = call(
        &p,
        &same_admin,
        "merge_branch",
        json!({ "name": "jr/work" }),
    )
    .await;
    assert_eq!(
        refused["issues"][0]["code"],
        json!("self_review"),
        "{refused}"
    );
    let admin = p.mint_token(TENANT, Role::Admin);
    let merged = call(&p, &admin, "merge_branch", json!({ "name": "jr/work" })).await;
    assert_eq!(merged["ok"], json!(true), "an admin merges: {merged}");
    // An admin's own branch waits for another admin.
    call(&p, &admin, "create_branch", json!({ "name": "admin/work" })).await;
    call(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": page("admins"), "content": body("admins"), "branch": "admin/work" }),
    )
    .await;
    let own_admin = call(&p, &admin, "merge_branch", json!({ "name": "admin/work" })).await;
    assert_eq!(
        own_admin["issues"][0]["code"],
        json!("self_review"),
        "{own_admin}"
    );
    let other_admin = p.mint_token_with_sub(TENANT, Role::Admin, "admin:two");
    let merged = call(
        &p,
        &other_admin,
        "merge_branch",
        json!({ "name": "admin/work" }),
    )
    .await;
    assert_eq!(merged["ok"], json!(true), "{merged}");
}

#[tokio::test]
async fn a_gateway_without_a_verifier_has_no_people_to_separate() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .done(),
        ),
    })
    .await;
    let open = |tool: &'static str, args: Value| {
        let url = p.mcp_url();
        async move {
            let body: Value = reqwest::Client::new()
                .post(url)
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": tool, "arguments": args } }))
                .send()
                .await
                .expect("post")
                .json()
                .await
                .expect("json");
            body["result"]["structuredContent"].clone()
        }
    };
    let made = open(
        "create_draft",
        json!({ "target_page_id": page("dev"), "content": body("dev") }),
    )
    .await;
    let id = made["draft"]["draft_id"]
        .as_str()
        .expect("draft")
        .to_owned();
    let promoted = open("promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(
        promoted["ok"],
        json!(true),
        "dev mode promotes its own draft: {promoted}"
    );
}

async fn start_signing() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            signing: true,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .done(),
        ),
    })
    .await
}

#[tokio::test]
async fn a_draft_an_agent_token_proposes_is_its_minters_own() {
    let p = start_signing().await;
    let jr = person(&p, GOOGLE, Some(&[ENTRA]));
    let admin = p.mint_token(TENANT, Role::Admin);
    // A workbench agent token (`mint_agent_token`) carries the person as its subject: the person is the author.
    let minted = call(
        &p,
        &jr,
        "mint_agent_token",
        json!({ "skill": "note", "ttl_secs": 600 }),
    )
    .await;
    let workbench = minted["token"].as_str().expect("token").to_owned();
    let by_workbench = draft(&p, &workbench, "by-workbench-agent").await;
    let stored = call(&p, &admin, "get_draft", json!({ "draft_id": by_workbench })).await;
    assert_eq!(stored["draft"]["author"], json!(GOOGLE), "{stored}");
    let own = call(
        &p,
        &jr,
        "promote_draft",
        json!({ "draft_id": by_workbench }),
    )
    .await;
    assert_eq!(own["issues"][0]["code"], json!("self_review"), "{own}");

    // A token minted for an agent (`agent:<id>` subject) names its minter in `act.sub`: the minter is an author.
    let agent = p.mint_token_acting_as(TENANT, "agent:scribe", GOOGLE);
    let filed = draft(&p, &agent, "by-agent").await;
    let stored = call(&p, &admin, "get_draft", json!({ "draft_id": filed })).await;
    assert_eq!(stored["draft"]["author"], json!("agent:scribe"), "{stored}");
    assert_eq!(stored["draft"]["minted_by"], json!(GOOGLE), "{stored}");
    // The minter, under either subject, does not accept what their agent proposed.
    for token in [jr.clone(), person(&p, ENTRA, Some(&[GOOGLE]))] {
        let own = call(&p, &token, "promote_draft", json!({ "draft_id": filed })).await;
        assert_eq!(own["issues"][0]["code"], json!("self_review"), "{own}");
    }
    // Nor does an admin who names the minter as the human who decided.
    let vouched = call(
        &p,
        &admin,
        "promote_draft",
        json!({ "draft_id": filed, "decided_by": GOOGLE }),
    )
    .await;
    assert_eq!(
        vouched["issues"][0]["code"],
        json!("self_review"),
        "{vouched}"
    );
    let eve = person(&p, EVE, Some(&[]));
    let accepted = call(&p, &eve, "promote_draft", json!({ "draft_id": filed })).await;
    assert_eq!(
        accepted["ok"],
        json!(true),
        "another person accepts: {accepted}"
    );
}

#[tokio::test]
async fn a_service_credential_decides_only_for_a_named_human_who_is_not_the_author() {
    let p = start().await;
    let jr = person(&p, GOOGLE, Some(&[ENTRA]));
    let runner = p.mint_token_with_groups(
        TENANT,
        "escurel-runner",
        &["escurel:runner", "editors"],
        false,
    );
    let filed = draft(&p, &jr, "for-service").await;
    let bare = call(&p, &runner, "promote_draft", json!({ "draft_id": filed })).await;
    assert_eq!(
        bare["issues"][0]["code"],
        json!("decided_by_required"),
        "{bare}"
    );
    let author = call(
        &p,
        &runner,
        "promote_draft",
        json!({ "draft_id": filed, "decided_by": GOOGLE }),
    )
    .await;
    assert_eq!(
        author["issues"][0]["code"],
        json!("self_review"),
        "{author}"
    );
    let named = call(
        &p,
        &runner,
        "promote_draft",
        json!({ "draft_id": filed, "decided_by": EVE }),
    )
    .await;
    assert_eq!(named["ok"], json!(true), "{named}");
    assert_eq!(named["decided_by"], json!(EVE), "{named}");

    // A branch: an admin service credential merges only for a named human who is not the author.
    call(&p, &jr, "create_branch", json!({ "name": "jr/svc" })).await;
    call(
        &p,
        &jr,
        "update_page",
        json!({ "page_id": page("svc"), "content": body("svc"), "branch": "jr/svc" }),
    )
    .await;
    let admin_service = p.mint_token_with_groups(
        TENANT,
        "escurel-runner",
        &["escurel:runner", "editors"],
        true,
    );
    let bare = call(
        &p,
        &admin_service,
        "merge_branch",
        json!({ "name": "jr/svc" }),
    )
    .await;
    assert_eq!(
        bare["issues"][0]["code"],
        json!("decided_by_required"),
        "{bare}"
    );
    let author = call(
        &p,
        &admin_service,
        "merge_branch",
        json!({ "name": "jr/svc", "decided_by": GOOGLE }),
    )
    .await;
    assert_eq!(
        author["issues"][0]["code"],
        json!("self_review"),
        "{author}"
    );
    // A service names a human, never an agent or itself.
    for bogus in ["agent:scribe", "escurel-runner", " "] {
        let refused = call(
            &p,
            &admin_service,
            "merge_branch",
            json!({ "name": "jr/svc", "decided_by": bogus }),
        )
        .await;
        assert_eq!(
            refused["issues"][0]["code"],
            json!("decided_by_required"),
            "{bogus}: {refused}"
        );
    }
    let merged = call(
        &p,
        &admin_service,
        "merge_branch",
        json!({ "name": "jr/svc", "decided_by": EVE }),
    )
    .await;
    assert_eq!(merged["ok"], json!(true), "{merged}");
    assert_eq!(
        merged["decided_by"],
        json!(EVE),
        "the human is recorded: {merged}"
    );
    let listed = call(
        &p,
        &p.mint_token(TENANT, Role::Admin),
        "list_branches",
        json!({}),
    )
    .await;
    let row = listed["branches"]
        .as_array()
        .expect("branches")
        .iter()
        .find(|b| b["name"] == json!("jr/svc"))
        .cloned()
        .expect("branch");
    assert_eq!(row["decided_by"], json!(EVE), "{row}");

    // A service promoting for an agent subject is refused the same way.
    let other = draft(&p, &jr, "for-service-2").await;
    let agentish = call(
        &p,
        &runner,
        "promote_draft",
        json!({ "draft_id": other, "decided_by": "agent:scribe" }),
    )
    .await;
    assert_eq!(
        agentish["issues"][0]["code"],
        json!("decided_by_required"),
        "{agentish}"
    );
}

#[tokio::test]
async fn a_branch_an_agent_token_opens_is_its_minters_too() {
    let p = start().await;
    let admin_h = p.mint_token_with_sub(TENANT, Role::Admin, "admin:h");
    let agent = p.mint_token_acting_as(TENANT, "agent:scribe", "admin:h");
    let made = call(&p, &agent, "create_branch", json!({ "name": "agent/work" })).await;
    assert_eq!(made["ok"], json!(true), "{made}");
    assert_eq!(made["branch"]["minted_by"], json!("admin:h"), "{made}");
    call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("agent-branch"), "content": body("agent-branch"), "branch": "agent/work" }),
    )
    .await;
    let own = call(
        &p,
        &admin_h,
        "merge_branch",
        json!({ "name": "agent/work" }),
    )
    .await;
    assert_eq!(
        own["issues"][0]["code"],
        json!("self_review"),
        "the minter does not merge it: {own}"
    );
    let other = p.mint_token_with_sub(TENANT, Role::Admin, "admin:k");
    let merged = call(&p, &other, "merge_branch", json!({ "name": "agent/work" })).await;
    assert_eq!(merged["ok"], json!(true), "{merged}");
}
