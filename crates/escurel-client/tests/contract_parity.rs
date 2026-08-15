//! Contract-parity tests: the typed client must carry every
//! optional-with-meaning field the server's tools accept and emit.
//!
//! Real gateway via `escurel-test-support` (real DuckDB, real MCP-over-HTTP,
//! real OIDC test issuer) — no mocks at the boundary (CLAUDE principle 2).
//!
//! These pin the drift findings where `escurel-types` / `escurel-client`
//! lagged the wire surface: dropped `as_of`/`scenario` on reads, the
//! missing `list_instances` cursor, and the missing CAS/approve guard on
//! `update_page` (`base_version` / `require_exact_base` / `base_sha256`)
//! plus the guard fields `expand` publishes (`version`, `content_sha256`).

use escurel_client::{
    Client, ExpandRequest, ListInstancesRequest, NeighboursRequest, ResolveRequest, SearchRequest,
    SecretString, UpdatePageRequest,
};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
optional_frontmatter: [tier, at]\n\
---\n\
# customer\n";

// The instances carry an `at:` timestamp because the `as_of` time-travel
// cut keys on it (`at_ts <= as_of`; UNTIMED pages always remain — they
// are not events on the timeline). A cut before `at` hides them.
const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: gold\n\
at: 2024-06-01T00:00:00Z\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";

const INITECH_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: initech\n\
name: Initech\n\
at: 2024-06-01T00:00:00Z\n\
---\n\
# Initech\n";

/// An RFC 3339 cut that predates the fixtures' `at:` timestamps.
const BEFORE_EVERYTHING: &str = "2000-01-01T00:00:00Z";

fn fixtures() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("customer", CUSTOMER_SKILL)
        .instance("customer", "acme", ACME_INSTANCE)
        .instance("customer", "initech", INITECH_INSTANCE)
        .done()
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures()),
        config_overrides: ConfigOverrides::default(),
    })
    .await
}

/// A gateway with the real DuckDB CRDT backend, the way the binary always
/// runs — required for the monotonic-version half of the guard loop.
async fn start_live() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures()),
        config_overrides: ConfigOverrides {
            live_crdt: true,
            ..Default::default()
        },
    })
    .await
}

async fn authed_client(p: &EscurelProcess) -> Client {
    let token = p.mint_token(TENANT, Role::Agent);
    Client::connect(p.base_url(), SecretString::from(token))
        .await
        .unwrap()
}

async fn resolve_page_id(client: &Client, wikilink: &str) -> String {
    client
        .resolve(ResolveRequest {
            wikilink: wikilink.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap()
        .page
        .expect("page present")
        .page_id
}

// ── item 4: dropped optional-with-meaning fields on reads ─────────

/// `expand` honours `as_of` server-side (a page born after the cut reads
/// as absent). The typed request has carried `as_of` all along — this
/// pins that the client actually SENDS it.
#[tokio::test]
async fn expand_forwards_as_of() {
    let p = start().await;
    let client = authed_client(&p).await;
    let page_id = resolve_page_id(&client, "[[customer::acme]]").await;

    // Plain read: present.
    let now = client
        .expand(ExpandRequest {
            page_id: page_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(now.page.is_some(), "plain expand finds the page");

    // Time-travel to before the fixture was written: absent.
    let past = client
        .expand(ExpandRequest {
            page_id,
            as_of: BEFORE_EVERYTHING.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        past.page.is_none(),
        "expand with as_of predating the write must resolve to no page — \
         the client dropped `as_of` on the floor"
    );
    p.shutdown().await;
}

/// Same plumbing pin for `neighbours`: edges from sources born after the
/// `as_of` cut are hidden server-side.
#[tokio::test]
async fn neighbours_forwards_as_of() {
    let p = start().await;
    let client = authed_client(&p).await;
    let page_id = resolve_page_id(&client, "[[customer::acme]]").await;

    let now = client
        .neighbours(NeighboursRequest {
            page_id: page_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !now.edges.is_empty(),
        "acme links to initech — plain neighbours sees the edge"
    );

    let past = client
        .neighbours(NeighboursRequest {
            page_id,
            as_of: BEFORE_EVERYTHING.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        past.edges.is_empty(),
        "neighbours with as_of predating the writes must see no edges — \
         the client dropped `as_of` on the floor"
    );
    p.shutdown().await;
}

/// Same plumbing pin for `list_instances`.
#[tokio::test]
async fn list_instances_forwards_as_of() {
    let p = start().await;
    let client = authed_client(&p).await;

    let now = client
        .list_instances(ListInstancesRequest {
            skill: "customer".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(now.instances.len(), 2, "both seeded instances visible");

    let past = client
        .list_instances(ListInstancesRequest {
            skill: "customer".to_owned(),
            as_of: BEFORE_EVERYTHING.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        past.instances.is_empty(),
        "list_instances with as_of predating the writes must be empty — \
         the client dropped `as_of` on the floor"
    );
    p.shutdown().await;
}

/// `search` already forwards `as_of` (fixed earlier) — pinned here so the
/// read surface stays uniform while the sibling methods catch up.
#[tokio::test]
async fn search_forwards_as_of() {
    let p = start().await;
    let client = authed_client(&p).await;
    // Scoped to instance pages: every seeded instance is timed, so the
    // pre-dating cut leaves no candidate blocks. (Unscoped search would
    // still return untimed SKILL blocks from the vector side.)
    let past = client
        .search(SearchRequest {
            q: "acme".to_owned(),
            page_type: "instance".to_owned(),
            as_of: BEFORE_EVERYTHING.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        past.hits.is_empty(),
        "as_of cut hides every timed instance block: {:?}",
        past.hits
    );
    p.shutdown().await;
}

// ── item 3: list_instances cursor pagination ──────────────────────

/// Paginate a real seeded instance list to exhaustion through the typed
/// client: request cursor + response next_cursor (only absent means done).
#[tokio::test]
async fn list_instances_paginates_to_exhaustion() {
    let mut fx = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("customer", CUSTOMER_SKILL);
    for i in 0..5 {
        let body = format!(
            "---\ntype: instance\nskill: customer\nid: c{i}\nname: Customer {i}\n---\n# C{i}\n"
        );
        fx = fx.instance("customer", &format!("c{i}"), body.as_str());
    }
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fx.done()),
        config_overrides: ConfigOverrides::default(),
    })
    .await;
    let client = authed_client(&p).await;

    let mut seen = Vec::new();
    let mut cursor = String::new();
    let mut pages = 0;
    loop {
        let resp = client
            .list_instances(ListInstancesRequest {
                skill: "customer".to_owned(),
                limit: 2,
                cursor: cursor.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            resp.instances.len() <= 2,
            "page respects the limit (got {})",
            resp.instances.len()
        );
        seen.extend(resp.instances.into_iter().map(|i| i.page_id));
        pages += 1;
        assert!(pages <= 10, "cursor loop must terminate");
        match resp.next_cursor {
            Some(c) => cursor = c,
            None => break,
        }
    }
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        5,
        "every seeded instance appears exactly once across pages: {seen:?}"
    );
    assert!(pages >= 3, "limit 2 over 5 rows takes at least 3 pages");
    p.shutdown().await;
}

// ── items 1 + 2: the read→hash→guarded-write loop ─────────────────

/// Full approve loop through the typed client: `expand` publishes
/// `content_sha256` (and, with a CRDT backend, `version`); a guarded
/// `update_page` carrying that hash succeeds; re-sending the now-stale
/// hash conflicts with `head_sha256`/`head_content` on the typed response.
#[tokio::test]
async fn expand_hash_feeds_guarded_update_and_stale_hash_conflicts() {
    let p = start_live().await;
    let client = authed_client(&p).await;
    let page_id = resolve_page_id(&client, "[[customer::acme]]").await;

    let read = client
        .expand(ExpandRequest {
            page_id: page_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let sha = read
        .content_sha256
        .clone()
        .expect("plain expand publishes content_sha256 (#354/#408)");
    assert_eq!(sha.len(), 64, "hex sha256");
    assert!(
        read.version.is_some(),
        "live-CRDT gateway publishes the monotonic version on expand (#246)"
    );

    // Guarded write against the hash we just read: succeeds.
    let updated = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: platinum\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";
    let ok = client
        .update_page(UpdatePageRequest {
            page_id: page_id.clone(),
            content: updated.to_owned(),
            base_sha256: Some(sha.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        ok.ok,
        "guarded write with the fresh hash lands: {:?}",
        ok.issues
    );

    // Same (now stale) hash again: refused as a typed conflict, with the
    // head hash + content for the approver to re-diff against.
    let stale = client
        .update_page(UpdatePageRequest {
            page_id: page_id.clone(),
            content:
                "---\ntype: instance\nskill: customer\nid: acme\nname: Acme Corp\n---\n# stale\n"
                    .to_owned(),
            base_sha256: Some(sha),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!stale.ok, "stale hash must not write");
    assert_eq!(stale.issues[0].code, "conflict");
    let head_sha = stale.head_sha256.expect("conflict carries head_sha256");
    assert_eq!(head_sha.len(), 64);
    let head = stale.head_content.expect("conflict carries head_content");
    assert!(
        head.contains("platinum"),
        "head_content is the landed write"
    );

    // The published hash tracks the head: reading again yields the hash the
    // conflict reported, closing the loop.
    let reread = client
        .expand(ExpandRequest {
            page_id,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(reread.content_sha256.as_deref(), Some(head_sha.as_str()));
    p.shutdown().await;
}

/// The version-CAS variant (#246): a strict (`require_exact_base`) write
/// against a stale `base_version` conflicts instead of auto-merging, and
/// the typed response carries `head_version`.
#[tokio::test]
async fn stale_base_version_with_require_exact_base_conflicts() {
    let p = start_live().await;
    let client = authed_client(&p).await;
    let page_id = resolve_page_id(&client, "[[customer::initech]]").await;

    let read = client
        .expand(ExpandRequest {
            page_id: page_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let base = read.version.expect("live-CRDT gateway publishes version");

    // Advance the head past the base we hold.
    let advance = "---\ntype: instance\nskill: customer\nid: initech\nname: Initech\ntier: silver\n---\n# Initech\n";
    let first = client
        .update_page(UpdatePageRequest {
            page_id: page_id.clone(),
            content: advance.to_owned(),
            base_version: Some(base.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(first.ok, "clean CAS write lands: {:?}", first.issues);
    assert_ne!(first.new_version, base, "version advanced");

    // Strict write against the stale base: conflict, nothing persisted.
    let stale = client
        .update_page(UpdatePageRequest {
            page_id: page_id.clone(),
            content: "---\ntype: instance\nskill: customer\nid: initech\nname: Initech\n---\n# strict loser\n".to_owned(),
            base_version: Some(base),
            require_exact_base: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!stale.ok, "strict stale base must conflict, never merge");
    assert_eq!(stale.issues[0].code, "conflict");
    assert_eq!(
        stale.head_version.as_deref(),
        Some(first.new_version.as_str()),
        "conflict reports the head the caller must re-read"
    );
    assert!(!stale.auto_merged, "strict path never auto-merges");

    let reread = client
        .expand(ExpandRequest {
            page_id,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        reread.body.contains("Initech") && !reread.body.contains("strict loser"),
        "the refused draft was not persisted"
    );
    p.shutdown().await;
}
