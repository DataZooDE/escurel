//! Server-stamped principal attribution on page writes and CRDT ops
//! (DataZooDE/escurel#357, CR-6).
//!
//! `capture_event` already stamps `provenance.captured_by` (#362). This file
//! covers the two halves that did not: `pages` (last writer) and `crdt_ops`
//! (op author), plus the read path for both.
//!
//! The property under test is not "a field is populated" — it is that the
//! value is the **gateway's** claim rather than the caller's. So every
//! positive assertion is paired with an attempt to forge, and every negative
//! assertion has a positive control: a second, differently-subjected token
//! writing through the same path must produce a *different* stamp, or an
//! implementation that hard-codes one principal would pass.
//!
//! Real gateway, real Indexer, real DuckDB, real JWKS (`AuthMode::TestIssuer`).
//! No mocks.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "consultant:alice";
const BOB: &str = "consultant:bob";

const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n---\n# note\n";
const NOTE_PAGE: &str = "markdown/instances/note/n1.md";

fn note_markdown(body: &str) -> String {
    format!("---\ntype: instance\nskill: note\nid: n1\n---\n# n1\n\n{body}\n")
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            // The CRDT half needs a real backend; the page half needs a real
            // indexer. Production wires both, so the test does too.
            live_crdt: true,
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
    resp.json().await.unwrap()
}

async fn call_ok(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = call(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name} errored: {body}");
    body["result"]["structuredContent"].clone()
}

/// The `last_written_by` `expand` reports for a page, or `None`.
async fn last_written_by(p: &EscurelProcess, token: &str, page_id: &str) -> Option<String> {
    let out = call_ok(p, token, "expand", json!({ "page_id": page_id })).await;
    out["page"]["last_written_by"].as_str().map(str::to_owned)
}

/// A persistent Loro peer, so successive ops are incremental updates
/// anchored to the last exported frontier. Mirrors `Client` in
/// `mcp_session_tools.rs`.
struct Peer {
    doc: LoroDoc,
    vv: loro::VersionVector,
}

impl Peer {
    fn new() -> Self {
        let doc = LoroDoc::new();
        let vv = doc.oplog_vv();
        Self { doc, vv }
    }

    fn insert(&mut self, text: &str) -> String {
        let pos = self.doc.get_text("body").len_unicode();
        self.doc.get_text("body").insert(pos, text).unwrap();
        self.doc.commit();
        let update = self.doc.export(ExportMode::updates(&self.vv)).unwrap();
        self.vv = self.doc.oplog_vv();
        B64.encode(update)
    }

    /// The Loro peer id — a *device*, not a person. The whole point of the
    /// crdt_ops half is that this is not an answer to "who edited".
    fn peer_id(&self) -> String {
        self.doc.peer_id().to_string()
    }
}

// ---------------------------------------------------------------- pages ---

/// An ordinary `update_page` persists the verified principal, and the caller
/// supplies nothing.
///
/// The positive control is Bob writing the same page through the same tool:
/// a stamp that is always "alice" (or always the first writer) is not
/// attribution, and would pass a single-subject assertion.
#[tokio::test]
async fn update_page_stamps_the_verified_caller_without_the_caller_supplying_it() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let r = call_ok(
        &p,
        &alice,
        "update_page",
        json!({ "page_id": NOTE_PAGE, "content": note_markdown("alice wrote this") }),
    )
    .await;
    assert_eq!(r["ok"], true, "alice's write must succeed: {r}");
    assert_eq!(
        last_written_by(&p, &alice, NOTE_PAGE).await.as_deref(),
        Some(ALICE),
        "update_page must stamp the verified caller"
    );

    // Positive control: the LAST writer wins, so Bob's write moves it.
    let r = call_ok(
        &p,
        &bob,
        "update_page",
        json!({ "page_id": NOTE_PAGE, "content": note_markdown("bob wrote this") }),
    )
    .await;
    assert_eq!(r["ok"], true, "bob's write must succeed: {r}");
    assert_eq!(
        last_written_by(&p, &alice, NOTE_PAGE).await.as_deref(),
        Some(BOB),
        "the stamp must follow the actual last writer, not the first"
    );

    p.shutdown().await;
}

/// The stamp cannot be overridden by anything the caller sends: not a
/// frontmatter key of the same name, not a `provenance` block, not a
/// top-level tool argument.
///
/// Positive control: the same forged content written by Bob stamps Bob — so
/// the assertion is testing the *source* of the value, not a constant.
#[tokio::test]
async fn caller_supplied_attribution_cannot_override_the_stamp() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let forged = "---\ntype: instance\nskill: note\nid: n1\n\
        last_written_by: \"consultant:mallory\"\nprincipal: \"consultant:mallory\"\n\
        ---\n# n1\n\nforged\n";

    let r = call_ok(
        &p,
        &alice,
        "update_page",
        json!({
            "page_id": NOTE_PAGE,
            "content": forged,
            // Every caller-controlled channel that could plausibly be read
            // as attribution, all naming someone else.
            "last_written_by": "consultant:mallory",
            "principal": "consultant:mallory",
            "provenance": { "last_written_by": "consultant:mallory",
                            "principal": "consultant:mallory" },
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "the write itself must still succeed: {r}");
    assert_eq!(
        last_written_by(&p, &alice, NOTE_PAGE).await.as_deref(),
        Some(ALICE),
        "a caller-supplied principal must be overwritten by the verified one"
    );

    // Positive control: the identical forged payload from Bob stamps BOB.
    let r = call_ok(
        &p,
        &bob,
        "update_page",
        json!({
            "page_id": NOTE_PAGE,
            "content": forged,
            "last_written_by": "consultant:mallory",
            "provenance": { "last_written_by": "consultant:mallory" },
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "bob's write must succeed: {r}");
    assert_eq!(
        last_written_by(&p, &alice, NOTE_PAGE).await.as_deref(),
        Some(BOB),
        "positive control: the stamp tracks the token, not the payload"
    );

    p.shutdown().await;
}

// ------------------------------------------------------------- crdt_ops ---

/// An op applied through `apply_op` is attributable to a **principal**, and
/// the two are not the same thing as the Loro peer id.
///
/// The construction is the point: ONE Loro peer (one device, one peer id)
/// produces both ops, and two DIFFERENT tokens apply them. Anything derived
/// from the op bytes would give the same answer twice. The recorded
/// principals differ, so the attribution comes from the gateway.
#[tokio::test]
async fn apply_op_attributes_the_op_to_a_principal_not_a_loro_peer_id() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let page = "markdown/instances/note/live.md";
    let opened = call_ok(&p, &alice, "open_session", json!({ "page_id": page })).await;
    let session = opened["session"].as_str().expect("session id").to_owned();

    let mut peer = Peer::new();
    let peer_id = peer.peer_id();

    let r = call_ok(
        &p,
        &alice,
        "apply_op",
        json!({ "session": session, "op": peer.insert("alice typed") }),
    )
    .await;
    assert_eq!(r["ok"], true, "alice's op must apply: {r}");

    let r = call_ok(
        &p,
        &bob,
        "apply_op",
        json!({ "session": session, "op": peer.insert(" and bob typed") }),
    )
    .await;
    assert_eq!(r["ok"], true, "bob's op must apply: {r}");

    let authors = call_ok(&p, &alice, "list_op_authors", json!({ "page_id": page })).await;
    let ops = authors["ops"].as_array().expect("ops array").clone();
    assert_eq!(ops.len(), 2, "one row per applied op: {authors}");

    let principals: Vec<&str> = ops
        .iter()
        .map(|o| o["principal"].as_str().unwrap_or("<null>"))
        .collect();
    assert_eq!(
        principals,
        vec![ALICE, BOB],
        "each op must carry the principal that applied it, in hlc order: {authors}"
    );
    assert!(
        !principals.iter().any(|s| *s == peer_id),
        "the principal must not be the Loro peer id ({peer_id}): {authors}"
    );

    p.shutdown().await;
}

/// A caller-supplied `principal` on `apply_op` is ignored; the gateway's
/// verified subject wins. Positive control in the same test: Bob's op,
/// forging Alice, still records Bob.
#[tokio::test]
async fn caller_supplied_op_principal_is_ignored() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let page = "markdown/instances/note/forge.md";
    let opened = call_ok(&p, &alice, "open_session", json!({ "page_id": page })).await;
    let session = opened["session"].as_str().expect("session id").to_owned();

    let mut peer = Peer::new();
    let r = call_ok(
        &p,
        &alice,
        "apply_op",
        json!({
            "session": session,
            "op": peer.insert("x"),
            "principal": "consultant:mallory",
            "author": "consultant:mallory",
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "the op must still apply: {r}");

    let r = call_ok(
        &p,
        &bob,
        "apply_op",
        json!({
            "session": session,
            "op": peer.insert("y"),
            "principal": ALICE,
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "bob's op must apply: {r}");

    let authors = call_ok(&p, &alice, "list_op_authors", json!({ "page_id": page })).await;
    let principals: Vec<&str> = authors["ops"]
        .as_array()
        .expect("ops array")
        .iter()
        .map(|o| o["principal"].as_str().unwrap_or("<null>"))
        .collect();
    assert_eq!(
        principals,
        vec![ALICE, BOB],
        "a forged `principal` must be overwritten by the verified subject: {authors}"
    );

    p.shutdown().await;
}
