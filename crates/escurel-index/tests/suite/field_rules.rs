//! Skill-declared validators (`field_rules`): required_when, non_blank,
//! `format: date`, immutability, link integrity with cycle detection,
//! composite identity and release hash binding. Real DuckDB and a real
//! `FsStore`; skills are neutral (`note`, `ticket`, `topic`).

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Issue, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _dirs: (TempDir, TempDir),
}

fn harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        store,
        indexer,
        _dirs: (store_dir, db_dir),
    }
}

async fn seed(h: &Harness, page_id: &str, body: &str) {
    let key = Key::new(TENANT, page_id.to_owned()).unwrap();
    h.store
        .write(&key, Bytes::copy_from_slice(body.as_bytes()))
        .await
        .unwrap();
    h.indexer.update_page(page_id, body).await.unwrap();
}

fn codes(issues: &[Issue]) -> Vec<&str> {
    issues.iter().map(|i| i.code.as_str()).collect()
}

fn has(issues: &[Issue], code: &str, location: &str) -> bool {
    issues
        .iter()
        .any(|i| i.code == code && i.location == location)
}

fn skill_page(id: &str, extra: &str) -> (String, String) {
    (
        format!("markdown/skills/{id}.md"),
        format!("---\nkind: skill\nid: {id}\ndescription: A {id}.\n{extra}---\n# {id}\n"),
    )
}

fn instance(skill: &str, id: &str, extra: &str) -> (String, String) {
    (
        format!("markdown/instances/{skill}/{id}.md"),
        format!("---\nkind: instance\nskill: {skill}\nid: {id}\n{extra}---\n# {id}\n"),
    )
}

async fn seed_all(h: &Harness, pages: &[(String, String)]) {
    for (id, body) in pages {
        seed(h, id, body).await;
    }
}

const TICKET_FIELDS: &str = "\
fields:
  - {name: state, kind: enum, values: [open, blocked, done]}
  - {name: reason, kind: string, required_when: {state: blocked}}
  - {name: title, kind: string, non_blank: true}
  - {name: due, kind: string, format: date}
  - {name: points, kind: int, min: 1, max: 13}
  - {name: opened, kind: string, immutable: true}
";

#[tokio::test]
async fn field_level_validators_report_generic_codes() {
    let h = harness();
    seed_all(&h, &[skill_page("ticket", TICKET_FIELDS)]).await;

    let (_, bad) = instance(
        "ticket",
        "t1",
        "state: nope\ntitle: \"  \"\ndue: 31.12.2026\npoints: 99\n",
    );
    let issues = h.indexer.validate(None, &bad).await.unwrap();
    assert!(
        has(&issues, "frontmatter_enum_value", "frontmatter.state"),
        "{issues:?}"
    );
    assert!(
        has(&issues, "field_blank", "frontmatter.title"),
        "{issues:?}"
    );
    assert!(
        has(&issues, "frontmatter_field_type", "frontmatter.due"),
        "{issues:?}"
    );
    assert!(
        has(&issues, "frontmatter_field_range", "frontmatter.points"),
        "{issues:?}"
    );

    let (_, ok) = instance(
        "ticket",
        "t1",
        "state: open\ntitle: Fix it\ndue: 2026-12-31\npoints: 3\n",
    );
    let issues = h.indexer.validate(None, &ok).await.unwrap();
    assert!(
        issues.is_empty(),
        "a conforming ticket is clean: {issues:?}"
    );
}

#[tokio::test]
async fn required_when_applies_only_while_the_condition_holds() {
    let h = harness();
    seed_all(&h, &[skill_page("ticket", TICKET_FIELDS)]).await;

    let (_, blocked) = instance("ticket", "t1", "state: blocked\n");
    let issues = h.indexer.validate(None, &blocked).await.unwrap();
    assert!(
        has(&issues, "field_required_when", "frontmatter.reason"),
        "{issues:?}"
    );

    let (_, blank) = instance("ticket", "t1", "state: blocked\nreason: \"\"\n");
    let issues = h.indexer.validate(None, &blank).await.unwrap();
    assert!(
        has(&issues, "field_required_when", "frontmatter.reason"),
        "blank is unmet: {issues:?}"
    );

    let (_, given) = instance("ticket", "t1", "state: blocked\nreason: waiting\n");
    assert!(
        !codes(&h.indexer.validate(None, &given).await.unwrap()).contains(&"field_required_when")
    );

    let (_, open) = instance("ticket", "t1", "state: open\n");
    assert!(
        !codes(&h.indexer.validate(None, &open).await.unwrap()).contains(&"field_required_when")
    );
}

#[tokio::test]
async fn strict_required_frontmatter_rejects_blank_required_keys() {
    let h = harness();
    seed_all(
        &h,
        &[skill_page(
            "note",
            "strict_required_frontmatter: true\nrequired_frontmatter: [title, owner]\n",
        )],
    )
    .await;
    let (_, page) = instance("note", "n1", "title: \"\"\n");
    let issues = h.indexer.validate(None, &page).await.unwrap();
    assert!(
        has(
            &issues,
            "frontmatter_required_key_blank_strict",
            "frontmatter.title"
        ),
        "{issues:?}"
    );
    assert!(
        has(
            &issues,
            "frontmatter_required_key_missing_strict",
            "frontmatter.owner"
        ),
        "{issues:?}"
    );
}

#[tokio::test]
async fn an_immutable_field_cannot_change_after_create() {
    let h = harness();
    let (tid, t1) = instance("ticket", "t1", "state: open\nopened: 2026-01-01\n");
    seed_all(
        &h,
        &[
            skill_page("ticket", TICKET_FIELDS),
            (tid.clone(), t1.clone()),
        ],
    )
    .await;

    // Same bytes: an idempotent rewrite is fine.
    assert!(
        h.indexer
            .validate(Some(&tid), &t1)
            .await
            .unwrap()
            .is_empty()
    );
    // A mutable field may change.
    let (_, edited) = instance("ticket", "t1", "state: done\nopened: 2026-01-01\n");
    assert!(
        h.indexer
            .validate(Some(&tid), &edited)
            .await
            .unwrap()
            .is_empty()
    );
    // The immutable one may not, nor be dropped.
    let (_, moved) = instance("ticket", "t1", "state: open\nopened: 2026-02-02\n");
    let issues = h.indexer.validate(Some(&tid), &moved).await.unwrap();
    assert!(
        has(&issues, "field_immutable", "frontmatter.opened"),
        "{issues:?}"
    );
    let (_, dropped) = instance("ticket", "t1", "state: open\n");
    let issues = h.indexer.validate(Some(&tid), &dropped).await.unwrap();
    assert!(
        has(&issues, "field_immutable", "frontmatter.opened"),
        "{issues:?}"
    );
    // A page that does not exist yet is a create, not a change.
    let (nid, new) = instance("ticket", "t2", "opened: 2026-03-03\n");
    assert!(
        !codes(&h.indexer.validate(Some(&nid), &new).await.unwrap()).contains(&"field_immutable")
    );
}

#[tokio::test]
async fn an_immutable_skill_freezes_its_instances_even_if_the_skill_key_is_edited() {
    let h = harness();
    let (eid, e1) = instance("event", "e1", "kind_of: login\n");
    seed_all(
        &h,
        &[
            skill_page("event", "immutable: true\n"),
            (eid.clone(), e1.clone()),
        ],
    )
    .await;

    assert!(
        h.indexer
            .validate(Some(&eid), &e1)
            .await
            .unwrap()
            .is_empty()
    );
    let (_, changed) = instance("event", "e1", "kind_of: logout\n");
    let issues = h.indexer.validate(Some(&eid), &changed).await.unwrap();
    assert!(has(&issues, "instance_immutable", "page_id"), "{issues:?}");

    // Re-labelling the page as another skill does not shed the rule: the path
    // and the stored page both still name `event`.
    let relabelled = "---\nkind: instance\nskill: note\nid: e1\nkind_of: logout\n---\n# e1\n";
    let issues = h.indexer.validate(Some(&eid), relabelled).await.unwrap();
    assert!(has(&issues, "instance_immutable", "page_id"), "{issues:?}");
}

const TOPIC_FIELDS: &str = "\
fields:
  - {name: parent, kind: link, to: topic, acyclic: true}
  - {name: owner, kind: link, to: person}
";

#[tokio::test]
async fn link_fields_resolve_to_an_instance_of_the_declared_skill() {
    let h = harness();
    let (_, root) = instance("topic", "root", "");
    let (_, person) = instance("person", "ann", "");
    seed_all(
        &h,
        &[
            skill_page("topic", TOPIC_FIELDS),
            skill_page("person", ""),
            instance("topic", "root", ""),
            instance("person", "ann", ""),
        ],
    )
    .await;
    let _ = (root, person);

    let (_, ok) = instance(
        "topic",
        "leaf",
        "parent: \"[[topic::root]]\"\nowner: \"[[person::ann]]\"\n",
    );
    assert!(h.indexer.validate(None, &ok).await.unwrap().is_empty());

    let (_, missing) = instance("topic", "leaf", "parent: \"[[topic::ghost]]\"\n");
    let issues = h.indexer.validate(None, &missing).await.unwrap();
    assert!(
        has(&issues, "relation_unresolved", "frontmatter.parent"),
        "{issues:?}"
    );

    let (_, wrong) = instance("topic", "leaf", "owner: \"[[topic::root]]\"\n");
    let issues = h.indexer.validate(None, &wrong).await.unwrap();
    assert!(
        has(&issues, "relation_invalid", "frontmatter.owner"),
        "{issues:?}"
    );

    let (_, prose) = instance("topic", "leaf", "parent: see root\n");
    let issues = h.indexer.validate(None, &prose).await.unwrap();
    assert!(
        has(&issues, "relation_invalid", "frontmatter.parent"),
        "{issues:?}"
    );
}

#[tokio::test]
async fn a_link_may_resolve_to_a_page_in_the_pending_batch() {
    let h = harness();
    seed_all(
        &h,
        &[skill_page("topic", TOPIC_FIELDS), skill_page("person", "")],
    )
    .await;

    let (_, leaf) = instance("topic", "leaf", "parent: \"[[topic::fresh]]\"\n");
    let issues = h.indexer.validate(None, &leaf).await.unwrap();
    assert!(
        has(&issues, "relation_unresolved", "frontmatter.parent"),
        "{issues:?}"
    );

    let (fid, fresh) = instance("topic", "fresh", "");
    let pending = HashMap::from([(fid, fresh)]);
    let issues = h
        .indexer
        .validate_with_pending(None, &leaf, &pending)
        .await
        .unwrap();
    assert!(
        !codes(&issues).contains(&"relation_unresolved"),
        "{issues:?}"
    );

    // An archived pending target does not count.
    let (aid, archived) = instance("topic", "fresh", "archived: true\n");
    let pending = HashMap::from([(aid, archived)]);
    let issues = h
        .indexer
        .validate_with_pending(None, &leaf, &pending)
        .await
        .unwrap();
    assert!(
        has(&issues, "relation_unresolved", "frontmatter.parent"),
        "{issues:?}"
    );
}

#[tokio::test]
async fn acyclic_links_reject_a_loop_through_saved_and_pending_pages() {
    let h = harness();
    seed_all(
        &h,
        &[
            skill_page("topic", TOPIC_FIELDS),
            instance("topic", "a", ""),
            instance("topic", "b", "parent: \"[[topic::a]]\"\n"),
        ],
    )
    .await;

    // Self link.
    let (_, selfie) = instance("topic", "c", "parent: \"[[topic::c]]\"\n");
    let pending = HashMap::from([instance("topic", "c", "")]);
    let issues = h
        .indexer
        .validate_with_pending(None, &selfie, &pending)
        .await
        .unwrap();
    assert!(
        has(&issues, "relation_cycle", "frontmatter.parent"),
        "{issues:?}"
    );

    // a -> b -> a: re-parenting `a` under `b` closes the loop through the store.
    let (aid, a_under_b) = instance("topic", "a", "parent: \"[[topic::b]]\"\n");
    let issues = h.indexer.validate(Some(&aid), &a_under_b).await.unwrap();
    assert!(
        has(&issues, "relation_cycle", "frontmatter.parent"),
        "{issues:?}"
    );

    // A pending page can close a loop too: x -> y (pending), y -> x.
    let (_, x) = instance("topic", "x", "parent: \"[[topic::y]]\"\n");
    let pending = HashMap::from([instance("topic", "y", "parent: \"[[topic::x]]\"\n")]);
    let issues = h
        .indexer
        .validate_with_pending(None, &x, &pending)
        .await
        .unwrap();
    assert!(
        has(&issues, "relation_cycle", "frontmatter.parent"),
        "{issues:?}"
    );

    // A chain is fine.
    let (_, d) = instance("topic", "d", "parent: \"[[topic::b]]\"\n");
    assert!(h.indexer.validate(None, &d).await.unwrap().is_empty());
}

const IDENT_FIELDS: &str = "\
fields:
  - {name: system, kind: string}
  - {name: ref, kind: string}
  - {name: status, kind: enum, values: [draft, accepted]}
unique:
  keys: [[system, ref]]
  scope: accepted
";

#[tokio::test]
async fn composite_identity_is_unique_among_accepted_instances() {
    let h = harness();
    let (id1, r1) = instance(
        "record",
        "r1",
        "system: erp\nref: \"42\"\nstatus: accepted\n",
    );
    let (_, draft) = instance("record", "r0", "system: erp\nref: \"42\"\nstatus: draft\n");
    let (did, _) = instance("record", "r0", "");
    seed_all(
        &h,
        &[
            skill_page("record", IDENT_FIELDS),
            (id1.clone(), r1.clone()),
            (did, draft),
        ],
    )
    .await;

    // A second accepted instance with the same (system, ref) collides.
    let (_, dup) = instance(
        "record",
        "r2",
        "system: erp\nref: \"42\"\nstatus: accepted\n",
    );
    let issues = h.indexer.validate(None, &dup).await.unwrap();
    assert!(
        has(&issues, "identity_duplicate", "frontmatter.system"),
        "{issues:?}"
    );

    // A different second key, or a draft, does not.
    let (_, other) = instance(
        "record",
        "r2",
        "system: erp\nref: \"43\"\nstatus: accepted\n",
    );
    assert!(h.indexer.validate(None, &other).await.unwrap().is_empty());
    let (_, as_draft) = instance("record", "r2", "system: erp\nref: \"42\"\nstatus: draft\n");
    assert!(
        h.indexer
            .validate(None, &as_draft)
            .await
            .unwrap()
            .is_empty()
    );

    // Re-validating the holder itself excludes self.
    assert!(
        h.indexer
            .validate(Some(&id1), &r1)
            .await
            .unwrap()
            .is_empty()
    );

    // Two accepted members of one pending batch collide with each other.
    let pending = HashMap::from([instance(
        "record",
        "r3",
        "system: crm\nref: \"7\"\nstatus: accepted\n",
    )]);
    let (_, r4) = instance(
        "record",
        "r4",
        "system: crm\nref: \"7\"\nstatus: accepted\n",
    );
    let issues = h
        .indexer
        .validate_with_pending(None, &r4, &pending)
        .await
        .unwrap();
    assert!(
        has(&issues, "identity_duplicate", "frontmatter.system"),
        "{issues:?}"
    );
}

#[tokio::test]
async fn unique_without_a_scope_counts_every_instance() {
    let h = harness();
    let (id1, r1) = instance("handle", "h1", "name: alpha\n");
    seed_all(&h, &[skill_page("handle", "unique: [[name]]\n"), (id1, r1)]).await;
    let (_, dup) = instance("handle", "h2", "name: alpha\n");
    let issues = h.indexer.validate(None, &dup).await.unwrap();
    assert!(
        has(&issues, "identity_duplicate", "frontmatter.name"),
        "{issues:?}"
    );
}

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[tokio::test]
async fn a_page_claiming_a_release_must_match_the_declared_hash() {
    let h = harness();
    seed_all(
        &h,
        &[skill_page(
            "catalogue",
            &format!("pack: {{id: starter, version: \"2026-01-01\", sha256: {HASH_A}}}\n"),
        )],
    )
    .await;

    let claim = |hash: &str, version: &str| {
        instance(
            "note",
            "n1",
            &format!("pack_id: starter\npack_version: \"{version}\"\npack_sha256: {hash}\n"),
        )
        .1
    };
    let ok = h
        .indexer
        .validate(None, &claim(HASH_A, "2026-01-01"))
        .await
        .unwrap();
    assert!(!ok.iter().any(|i| i.code.starts_with("pack_")), "{ok:?}");

    let bad = h
        .indexer
        .validate(None, &claim(HASH_B, "2026-01-01"))
        .await
        .unwrap();
    assert!(
        has(&bad, "pack_hash_mismatch", "frontmatter.pack_sha256"),
        "{bad:?}"
    );

    let unknown = h
        .indexer
        .validate(None, &claim(HASH_A, "2027-01-01"))
        .await
        .unwrap();
    assert!(
        has(&unknown, "pack_unknown", "frontmatter.pack_id"),
        "{unknown:?}"
    );

    let (_, partial) = instance("note", "n1", "pack_id: starter\n");
    let issues = h.indexer.validate(None, &partial).await.unwrap();
    assert!(
        has(&issues, "pack_malformed", "frontmatter.pack_id"),
        "{issues:?}"
    );

    // A pending skill page can declare the release too.
    let (pid, decl) = skill_page(
        "extras",
        &format!("pack: {{id: extra, version: \"1\", sha256: {HASH_B}}}\n"),
    );
    let pending = HashMap::from([(pid, decl)]);
    let (_, page) = instance(
        "note",
        "n1",
        &format!("pack_id: extra\npack_version: \"1\"\npack_sha256: {HASH_B}\n"),
    );
    let issues = h
        .indexer
        .validate_with_pending(None, &page, &pending)
        .await
        .unwrap();
    assert!(
        !issues.iter().any(|i| i.code.starts_with("pack_")),
        "{issues:?}"
    );
}

#[tokio::test]
async fn malformed_rule_declarations_are_reported_on_the_skill_page() {
    let h = harness();
    let (_, bad_pack) = skill_page("c1", "pack: {id: x, version: \"1\", sha256: nothex}\n");
    let issues = h.indexer.validate(None, &bad_pack).await.unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i.code == "skill_rule_malformed" && i.location == "frontmatter.pack[0]"),
        "{issues:?}"
    );

    let (_, bad_unique) = skill_page("c2", "unique: yes please\n");
    let issues = h.indexer.validate(None, &bad_unique).await.unwrap();
    assert!(
        has(&issues, "skill_rule_malformed", "frontmatter.unique"),
        "{issues:?}"
    );

    let (_, bad_scope) = skill_page("c3", "unique: {keys: [[a]], scope: everyone}\n");
    let issues = h.indexer.validate(None, &bad_scope).await.unwrap();
    assert!(
        has(&issues, "skill_rule_malformed", "frontmatter.unique"),
        "{issues:?}"
    );

    let (_, bad_immutable) = skill_page("c4", "immutable: sometimes\n");
    let issues = h.indexer.validate(None, &bad_immutable).await.unwrap();
    assert!(
        has(&issues, "skill_rule_malformed", "frontmatter.immutable"),
        "{issues:?}"
    );

    let (_, bad_field) = skill_page(
        "c5",
        "fields:\n  - {name: a, kind: string, acyclic: true}\n  - {name: b, kind: string, required_when: {x: 1, y: 2}}\n  - {name: d, kind: string, immutable: maybe}\n",
    );
    let issues = h.indexer.validate(None, &bad_field).await.unwrap();
    assert!(
        has(&issues, "fields_malformed", "frontmatter.fields.a.acyclic"),
        "{issues:?}"
    );
    assert!(
        has(
            &issues,
            "fields_malformed",
            "frontmatter.fields.b.required_when"
        ),
        "{issues:?}"
    );
    assert!(
        has(
            &issues,
            "fields_malformed",
            "frontmatter.fields.d.immutable"
        ),
        "{issues:?}"
    );

    let (_, fine) = skill_page(
        "c6",
        "unique: [[a, b]]\nfields:\n  - {name: a, kind: string}\n  - {name: b, kind: string}\n",
    );
    let issues = h.indexer.validate(None, &fine).await.unwrap();
    assert!(
        issues
            .iter()
            .all(|i| i.severity == escurel_index::Severity::Warning),
        "{issues:?}"
    );
}

#[tokio::test]
async fn skills_without_declared_rules_are_unchanged() {
    let h = harness();
    seed_all(&h, &[skill_page("plain", "")]).await;
    let (id, page) = instance("plain", "p1", "pack_note: x\n");
    seed(&h, &id, &page).await;
    let (_, edited) = instance(
        "plain",
        "p1",
        "pack_note: y\nparent: \"[[plain::ghost]]\"\n",
    );
    let issues = h.indexer.validate(Some(&id), &edited).await.unwrap();
    assert!(
        issues
            .iter()
            .all(|i| i.severity == escurel_index::Severity::Warning),
        "an undeclared skill gets no new errors: {issues:?}"
    );
}
