//! Integration tests for `Indexer::validate` (dry-run authoring
//! checks). Real DuckDB + real FsStore, no mocks. These pin the
//! exact issue set produced for a draft that references several
//! skills — some indexed, some not — so the batched single-pass
//! skill resolution stays behaviourally identical to the old
//! per-wikilink query path.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator, Severity};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     required_frontmatter:\n\
       - tier\n\
       - status\n\
     ---\n\
     # customer\n",
);

const SKILL_MEETING: (&str, &str) = (
    "markdown/skills/meeting.md",
    "---\n\
     type: skill\n\
     id: meeting\n\
     description: A meeting.\n\
     ---\n\
     # meeting\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let duckdb_path = db_dir.path().join("escurel.duckdb");

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();

    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
}

#[tokio::test]
async fn validate_clean_draft_has_no_issues() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let draft = "---\n\
                 type: instance\n\
                 skill: customer\n\
                 id: acme\n\
                 tier: enterprise\n\
                 status: active\n\
                 ---\n\
                 # Acme\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert!(issues.is_empty(), "{issues:?}");
}

#[tokio::test]
async fn validate_batches_mixed_wikilink_skills_with_identical_issue_set() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, SKILL_MEETING]).await;

    // Draft references: customer (exists), meeting (exists, twice),
    // vendor (unknown), project (unknown), plus an empty-id typed
    // link and a bare link (no skill). It also declares skill:
    // customer but omits the required `status` key.
    let draft = "---\n\
                 type: instance\n\
                 skill: customer\n\
                 id: acme\n\
                 tier: enterprise\n\
                 ---\n\
                 # Acme\n\
                 Linked to [[customer::globex]] and [[meeting::qbr]].\n\
                 Also [[meeting::renewal]] and [[vendor::aws]].\n\
                 And [[project::atlas]] plus [[customer::]] and [[bare-id]].\n";

    let issues = h.indexer.validate(None, draft).await.unwrap();

    // Required-key miss: status (customer requires tier+status; tier present).
    let required_misses: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "frontmatter_required_key_missing")
        .collect();
    assert_eq!(required_misses.len(), 1, "{issues:?}");
    assert_eq!(required_misses[0].location, "frontmatter.status");
    assert_eq!(required_misses[0].severity, Severity::Error);

    // Unknown-skill errors: vendor + project (customer/meeting exist).
    let mut unknown: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "unknown_skill")
        .map(|i| i.message.clone())
        .collect();
    unknown.sort();
    assert_eq!(unknown.len(), 2, "{issues:?}");
    assert!(unknown[0].contains("project"), "{unknown:?}");
    assert!(unknown[1].contains("vendor"), "{unknown:?}");

    // Empty-id typed wikilink: one wikilink_parse warning.
    let parse_warns: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "wikilink_parse")
        .collect();
    assert_eq!(parse_warns.len(), 1, "{issues:?}");
    assert_eq!(parse_warns[0].severity, Severity::Warning);

    // Dangling-target warnings: this fixture seeds skills but no
    // instances, so every resolvable-skill link points at nothing.
    // Warnings, not errors — none of them sits in a required field.
    let dangling: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "dangling_wikilink")
        .collect();
    assert_eq!(dangling.len(), 3, "{issues:?}");
    assert!(
        dangling.iter().all(|i| i.severity == Severity::Warning),
        "{issues:?}"
    );

    // Total issue count is exactly these seven.
    assert_eq!(issues.len(), 7, "unexpected extra issues: {issues:?}");
}

#[tokio::test]
async fn validate_instance_with_unknown_declared_skill_errors() {
    let h = fresh_harness();
    // No skills seeded.
    let draft = "---\n\
                 type: instance\n\
                 skill: ghost\n\
                 id: x\n\
                 ---\n\
                 # X\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].code, "unknown_skill");
    assert_eq!(issues[0].location, "frontmatter.skill");
}

// ── write-path validation gaps found by testing a live tenant ──────
//
// Three writes that should have been refused were accepted, each
// surfacing as damage somewhere else later:
//
//   * a frontmatter wikilink was never examined at all — only body
//     links were — so `about: "[[nosuchskill::x]]"` passed while the
//     same link in the body was rejected;
//   * no wikilink target was ever resolved, so an agent could name a
//     customer that does not exist and the graph would carry the
//     dangling edge;
//   * an instance with no `id:` was accepted, producing a page that
//     lists but cannot be expanded or resolved.

const SKILL_OFFER: (&str, &str) = (
    "markdown/skills/offer.md",
    "---\n\
     type: skill\n\
     id: offer\n\
     description: A quote.\n\
     required_frontmatter:\n\
       - customer\n\
     ---\n\
     # offer\n",
);

const INSTANCE_ACME: (&str, &str) = (
    "markdown/instances/customer/acme.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme\n\
     tier: enterprise\n\
     status: active\n\
     ---\n\
     # Acme\n",
);

fn codes(issues: &[escurel_index::Issue]) -> Vec<&str> {
    issues.iter().map(|i| i.code.as_str()).collect()
}

/// A wikilink in FRONTMATTER must be checked like one in the body.
#[tokio::test]
async fn unknown_skill_in_frontmatter_is_rejected_like_one_in_the_body() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let draft = "---\n\
                 type: instance\n\
                 skill: customer\n\
                 id: acme\n\
                 tier: enterprise\n\
                 status: active\n\
                 about: \"[[nosuchskill::x]]\"\n\
                 ---\n\
                 # Acme\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert!(
        codes(&issues).contains(&"unknown_skill"),
        "a frontmatter wikilink must be validated too: {issues:?}"
    );
}

/// A dangling target in a REQUIRED field is an error: that is the
/// hallucinated-customer case, and the one nobody re-checks.
#[tokio::test]
async fn dangling_target_in_a_required_field_is_an_error() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, SKILL_OFFER, INSTANCE_ACME]).await;

    let draft = "---\n\
                 type: instance\n\
                 skill: offer\n\
                 id: an26-9999\n\
                 customer: \"[[customer::totally-made-up-gmbh]]\"\n\
                 ---\n\
                 # Offer\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "dangling_wikilink" && i.severity == Severity::Error),
        "a required-field link that resolves to nothing must be an error: {issues:?}"
    );

    // ...and the same field pointing at a real page is clean.
    let good = "---\n\
                type: instance\n\
                skill: offer\n\
                id: an26-9999\n\
                customer: \"[[customer::acme]]\"\n\
                ---\n\
                # Offer\n";
    let issues = h.indexer.validate(None, good).await.unwrap();
    assert!(issues.is_empty(), "a resolvable link is clean: {issues:?}");
}

/// Everywhere else a dangling target is a WARNING, not an error.
///
/// Forward references are legitimate in a second brain and the tenant
/// depends on them: a meeting's `continues:` was written pointing at
/// the earlier session before that page existed. Hard-rejecting would
/// have broken it.
#[tokio::test]
async fn dangling_target_outside_a_required_field_only_warns() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, SKILL_MEETING]).await;

    let draft = "---\n\
                 type: instance\n\
                 skill: customer\n\
                 id: acme\n\
                 tier: enterprise\n\
                 status: active\n\
                 continues: \"[[meeting::not-yet-written]]\"\n\
                 ---\n\
                 # Acme\n\n\
                 Body also cites [[customer::future-prospect]].\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();

    let dangling: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "dangling_wikilink")
        .collect();
    assert_eq!(dangling.len(), 2, "both links reported: {issues:?}");
    assert!(
        dangling.iter().all(|i| i.severity == Severity::Warning),
        "forward references warn, they do not block: {issues:?}"
    );
    assert!(
        !issues.iter().any(|i| i.severity == Severity::Error),
        "the draft is still writable: {issues:?}"
    );
}

/// An instance with no `id:` produced a page that listed but could not
/// be expanded (`invalid type: null, expected a string`) or resolved.
#[tokio::test]
async fn an_instance_without_an_id_is_rejected() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let draft = "---\n\
                 type: instance\n\
                 skill: customer\n\
                 tier: enterprise\n\
                 status: active\n\
                 ---\n\
                 # No id\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "frontmatter_required_key_missing"
                && i.location.contains("id")
                && i.severity == Severity::Error),
        "an instance needs an id: {issues:?}"
    );
}

/// **The boundary this design rests on.**
///
/// Validation lives in the *authoring* path (`validate`, and the
/// `update_page` MCP tool that calls it), never in
/// `Indexer::update_page` — because `rebuild` re-indexes every page in
/// the lane through that method, in arbitrary order. A page citing a
/// page not yet reindexed is normal there, so hard-failing on a
/// dangling link would break crash recovery: the corpus would refuse
/// to rebuild itself.
///
/// If someone later "tightens" validation by moving it into
/// `update_page`, this test is what fails.
#[tokio::test]
async fn rebuild_tolerates_dangling_links_that_authoring_would_flag() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, SKILL_MEETING]).await;

    // A page whose frontmatter cites a sibling that does not exist yet —
    // exactly the `continues:` forward reference a multi-session workshop
    // produces.
    let forward = "---\n\
                   type: instance\n\
                   skill: customer\n\
                   id: acme\n\
                   tier: enterprise\n\
                   status: active\n\
                   continues: \"[[meeting::written-later]]\"\n\
                   ---\n\
                   # Acme\n";
    let key = Key::new(TENANT, "markdown/instances/customer/acme.md".to_owned()).unwrap();
    h.store
        .write(&key, Bytes::from_static(forward.as_bytes()))
        .await
        .unwrap();
    h.indexer
        .update_page("markdown/instances/customer/acme.md", forward)
        .await
        .expect("the indexer write path must not enforce link targets");

    // Authoring flags it — as a warning, so it is still writable.
    let issues = h.indexer.validate(None, forward).await.unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "dangling_wikilink" && i.severity == Severity::Warning),
        "authoring surfaces the forward reference: {issues:?}"
    );

    // And a from-scratch rebuild succeeds regardless.
    h.indexer
        .rebuild()
        .await
        .expect("rebuild must not validate");
    let drift = h.indexer.audit().await.expect("audit");
    assert!(drift.is_clean(), "rebuild reconciles cleanly: {drift:?}");
}

/// **The reserved `skill::` namespace must validate, not just resolve (#424).**
///
/// `[[skill::<id>]]` is the documented way to reference a skill *definition*
/// page — `read.rs` calls the namespace reserved and `resolve` constrains on
/// `page_type = 'skill'` for it (#212). The validator did not know: it treated
/// `skill` as a skill id, looked for a skill page called `skill`, and refused
/// every page using the form with `unknown_skill`.
///
/// So a page that resolves correctly could not be WRITTEN. It surfaced from
/// Heron, whose workshop formats reference a shared procedure exactly this way
/// (BR-WS-2): its Rust tests seed through a fixture builder that writes
/// straight to the store, so validation never ran on them, and the first thing
/// to author a format the way a tenant would — an app test over the real write
/// path — was refused.
#[tokio::test]
async fn validate_accepts_the_reserved_skill_namespace() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let draft = "---\n\
                 type: skill\n\
                 id: onboarding\n\
                 description: References another skill's procedure.\n\
                 ---\n\
                 # onboarding\n\
                 Then follow [[skill::customer]].\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert!(
        issues.is_empty(),
        "a wikilink into the reserved `skill::` namespace must validate when \
         the referenced SKILL exists — `resolve` honours the namespace, so a \
         page that resolves must also be writable: {issues:?}"
    );
}

/// The control, and it is what keeps the exemption from being a hole: a
/// reserved-namespace link to a skill that does NOT exist is still an error.
///
/// Without this, exempting `skill::` from the check would turn every
/// mistyped skill reference into a silently dangling link — the failure the
/// `unknown_skill` issue exists to prevent, reintroduced through the fix for
/// its false positive.
#[tokio::test]
async fn validate_still_refuses_a_reserved_link_to_a_missing_skill() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let draft = "---\n\
                 type: skill\n\
                 id: onboarding\n\
                 description: References a skill that is not there.\n\
                 ---\n\
                 # onboarding\n\
                 Then follow [[skill::no_such_procedure]].\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert!(
        issues.iter().any(|i| i.code == "unknown_skill"),
        "a reserved-namespace link to a missing skill must still be an error, \
         or the fix for the false positive becomes a dangling-link hole: \
         {issues:?}"
    );
}
