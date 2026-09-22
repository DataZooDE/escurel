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

/// A skill page is not an instance of itself.
///
/// `required_frontmatter` says what a skill's INSTANCES must carry. The skill
/// page declaring it carries none of those fields and should not be expected
/// to: `customer` requires `tier` and `status`, which are facts about a
/// customer, not about the page that defines what a customer is.
///
/// Found while seeding a deployed corpus, where `page validate` reported
/// every capture skill as REJECTED — `markdown/skills/calendar.md` missing
/// `at`, `source` and `channel` — for content `page update` then accepted
/// without complaint. A dry run that disagrees with the real write is worse
/// than none, because it teaches people to ignore validation output.
#[tokio::test]
async fn a_skill_page_does_not_have_to_satisfy_its_own_required_frontmatter() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    // The seeded skill itself, re-validated exactly as a seed script does.
    let issues = h.indexer.validate(None, SKILL_CUSTOMER.1).await.unwrap();
    assert!(
        !issues
            .iter()
            .any(|i| i.code == "frontmatter_required_key_missing"),
        "a skill page must not be held to the rules it sets for its \
         instances: {issues:?}"
    );

    // POSITIVE CONTROL: an INSTANCE of that skill missing the same keys IS
    // still an error, so the assertion above is about the page type and not
    // about the check having been switched off.
    let instance = "---\n\
                    type: instance\n\
                    skill: customer\n\
                    id: acme\n\
                    ---\n\
                    # Acme\n";
    let issues = h.indexer.validate(None, instance).await.unwrap();
    let missing: Vec<&str> = issues
        .iter()
        .filter(|i| i.code == "frontmatter_required_key_missing")
        .map(|i| i.location.as_str())
        .collect();
    assert!(
        missing.contains(&"frontmatter.tier") && missing.contains(&"frontmatter.status"),
        "control: an instance must still be held to them: {issues:?}"
    );
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
    // A skill page without a `summary:` draws the workbench's
    // `summary_missing` WARNING (P2-7); what this test guards is that
    // nothing here is an error.
    let errors: Vec<_> = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
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

// ── Typed skill fields (#508) ────────────────────────────────────────────
//
// `required_frontmatter` is a KEY-NAME list: it says `hotness` must be
// present and nothing about what may be in it, so `hotness: 5-Cold-ish`
// commits clean and `list_instances(filter={hotness: cold})` silently
// fractures the corpus into synonym classes. `fields:` adds the shape.

const SKILL_TYPED: (&str, &str) = (
    "markdown/skills/account.md",
    "---\n\
     type: skill\n\
     id: account\n\
     description: A customer account.\n\
     fields:\n\
       - {name: hotness, kind: enum, values: [hot, warm, cold]}\n\
       - {name: opened,  kind: date, required: true}\n\
       - {name: arr_eur, kind: float, min: 0}\n\
       - {name: seats,   kind: int}\n\
       - {name: active,  kind: bool}\n\
       - {name: segment, kind: string}\n\
     ---\n\
     # account\n",
);

fn typed_instance(body: &str) -> String {
    format!("---\ntype: instance\nskill: account\nid: globex\n{body}---\n# Globex\n")
}

/// The article's showcase, reproduced: an agent physically cannot write
/// `5-Cold-ish` into a field declared as an enum.
#[tokio::test]
async fn a_value_outside_a_declared_enum_is_an_error() {
    let h = fresh_harness();
    seed(&h, &[SKILL_TYPED]).await;

    let issues = h
        .indexer
        .validate(
            None,
            &typed_instance("hotness: 5-Cold-ish\nopened: 2026-01-05\n"),
        )
        .await
        .unwrap();
    let bad = issues
        .iter()
        .find(|i| i.code == "frontmatter_enum_value")
        .unwrap_or_else(|| panic!("an out-of-enum value must be reported: {issues:?}"));
    assert_eq!(bad.severity, Severity::Error, "{bad:?}");
    assert_eq!(bad.location, "frontmatter.hotness", "{bad:?}");
    assert!(
        bad.message.contains("hot") && bad.message.contains("warm") && bad.message.contains("cold"),
        "the message must name the allowed set — a rejection that does not \
         say what IS allowed costs the author another round trip: {bad:?}"
    );

    // Control: a declared value passes, so the check is about membership
    // and not about the field being rejected outright.
    let issues = h
        .indexer
        .validate(None, &typed_instance("hotness: cold\nopened: 2026-01-05\n"))
        .await
        .unwrap();
    assert!(
        !issues.iter().any(|i| i.code == "frontmatter_enum_value"),
        "a declared value must pass: {issues:?}"
    );
}

/// Every other kind in the closed vocabulary, positive and negative, in one
/// place — so a kind that stops being enforced fails here rather than
/// silently widening what an agent may write.
#[tokio::test]
async fn a_value_that_does_not_parse_as_its_declared_kind_is_an_error() {
    let h = fresh_harness();
    seed(&h, &[SKILL_TYPED]).await;

    for (body, key) in [
        ("opened: last tuesday\n", "frontmatter.opened"),
        ("opened: 2026-01-05\nseats: twelve\n", "frontmatter.seats"),
        ("opened: 2026-01-05\narr_eur: lots\n", "frontmatter.arr_eur"),
        (
            "opened: 2026-01-05\nactive: yesplease\n",
            "frontmatter.active",
        ),
    ] {
        let issues = h
            .indexer
            .validate(None, &typed_instance(body))
            .await
            .unwrap();
        let bad = issues
            .iter()
            .find(|i| i.code == "frontmatter_field_type" && i.location == key)
            .unwrap_or_else(|| panic!("{key} must be reported for {body:?}: {issues:?}"));
        assert_eq!(bad.severity, Severity::Error, "{bad:?}");
    }

    // The whole positive row: every kind, a value that fits it.
    let ok = typed_instance(
        "opened: 2026-01-05\nseats: 12\narr_eur: 99000.50\nactive: true\n\
         hotness: warm\nsegment: mid-market\n",
    );
    let issues = h.indexer.validate(None, &ok).await.unwrap();
    assert!(
        !issues.iter().any(|i| i.severity == Severity::Error),
        "a fully well-typed instance must validate clean: {issues:?}"
    );
}

/// A range is part of the shape: `arr_eur: min 0` exists to keep a negative
/// number out, and a declared bound that is not enforced is decoration.
#[tokio::test]
async fn a_value_outside_a_declared_range_is_an_error() {
    let h = fresh_harness();
    seed(&h, &[SKILL_TYPED]).await;

    let issues = h
        .indexer
        .validate(None, &typed_instance("opened: 2026-01-05\narr_eur: -5\n"))
        .await
        .unwrap();
    let bad = issues
        .iter()
        .find(|i| i.code == "frontmatter_field_range")
        .unwrap_or_else(|| panic!("a below-min value must be reported: {issues:?}"));
    assert_eq!(bad.location, "frontmatter.arr_eur", "{bad:?}");
    assert_eq!(bad.severity, Severity::Error, "{bad:?}");
    assert!(bad.message.contains('0'), "name the bound: {bad:?}");
}

/// `fields[].required` is about presence, like `required_frontmatter`, and
/// reported with the SAME code — a reviewer should not have to learn two
/// vocabularies for one missing key.
#[tokio::test]
async fn a_missing_required_field_is_reported_as_a_missing_key() {
    let h = fresh_harness();
    seed(&h, &[SKILL_TYPED]).await;

    let issues = h
        .indexer
        .validate(None, &typed_instance("hotness: hot\n"))
        .await
        .unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "frontmatter_required_key_missing"
                && i.location == "frontmatter.opened"),
        "a required declared field that is absent must be reported: {issues:?}"
    );
    // An OPTIONAL declared field that is absent is not a finding: declaring
    // a field is not the same as demanding it.
    assert!(
        !issues.iter().any(|i| i.location == "frontmatter.segment"),
        "an absent optional field is not a problem: {issues:?}"
    );
}

/// The skill author's own mistakes, caught on the skill page rather than on
/// every instance of it.
#[tokio::test]
async fn a_malformed_fields_block_is_reported_on_the_skill_page() {
    let h = fresh_harness();

    let nameless = "---\ntype: skill\nid: broken\nfields:\n  - {kind: enum}\n---\n# broken\n";
    let issues = h.indexer.validate(None, nameless).await.unwrap();
    let bad = issues
        .iter()
        .find(|i| i.code == "fields_malformed")
        .unwrap_or_else(|| panic!("a field with no name cannot be enforced: {issues:?}"));
    assert_eq!(bad.severity, Severity::Error, "{bad:?}");

    let scalar = "---\ntype: skill\nid: broken\nfields: hotness\n---\n# broken\n";
    assert!(
        h.indexer
            .validate(None, scalar)
            .await
            .unwrap()
            .iter()
            .any(|i| i.code == "fields_malformed"),
        "a scalar `fields:` is not a schema"
    );

    // An unknown kind DEGRADES to string with a warning rather than erroring:
    // the same fallback direction `ParamKind` chose, and for the same reason —
    // an over-permissive field under-validates, a dropped one loses data.
    let odd = "---\ntype: skill\nid: odd\nfields:\n  - {name: x, kind: uuid}\n---\n# odd\n";
    let issues = h.indexer.validate(None, odd).await.unwrap();
    let warn = issues
        .iter()
        .find(|i| i.code == "field_kind_unknown")
        .unwrap_or_else(|| panic!("an unknown kind must be reported: {issues:?}"));
    assert_eq!(warn.severity, Severity::Warning, "{warn:?}");
    assert!(
        !issues.iter().any(|i| i.severity == Severity::Error),
        "and must not block the skill page: {issues:?}"
    );
}

/// The regression that matters most: a skill with no `fields:` block behaves
/// exactly as it did. Typing is opt-in per skill — declaring the block IS
/// the migration step, which is why enforcement can be error-severity from
/// the first release without breaking a corpus written untyped.
#[tokio::test]
async fn a_skill_without_fields_is_unchanged() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let instance = "---\n\
                    type: instance\n\
                    skill: customer\n\
                    id: acme\n\
                    tier: 5-Cold-ish\n\
                    status: whatever-you-like\n\
                    ---\n\
                    # Acme\n";
    let issues = h.indexer.validate(None, instance).await.unwrap();
    assert!(
        !issues.iter().any(|i| matches!(
            i.code.as_str(),
            "frontmatter_field_type" | "frontmatter_enum_value" | "frontmatter_field_range"
        )),
        "an untyped skill types nothing: {issues:?}"
    );
    assert!(
        !issues.iter().any(|i| i.severity == Severity::Error),
        "and still accepts what it always accepted: {issues:?}"
    );
}
