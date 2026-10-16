//! `Indexer::migrate_kind_renaming` (`migrate-kind --rename-field OLD=NEW`): a tenant's own domain
//! field that collides with the page-kind key is renamed INSIDE the migration, so heron's two
//! `system` pages (`type: instance` + `kind: bus`) no longer block it and no live page has to be
//! hand-edited first. Real DuckDB + real file store + real Loro snapshots, no mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::drafts::NewDraft;
use escurel_index::{Indexer, Migrator};
use escurel_md::FieldRename;
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "datazoo-loops";

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh() -> Harness {
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
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

fn key(path: &str) -> Key {
    Key::new(TENANT, path.to_owned()).unwrap()
}

/// An OLD store: the legacy spelling lives in the lane, not in the index.
async fn put(h: &Harness, path: &str, md: &str) {
    h.store
        .write(&key(path), Bytes::from(md.to_owned()))
        .await
        .unwrap();
}

async fn lane(h: &Harness, path: &str) -> String {
    String::from_utf8(h.store.read(&key(path)).await.unwrap().to_vec()).unwrap()
}

fn renames() -> Vec<FieldRename> {
    vec![FieldRename::parse("kind=system_kind").unwrap()]
}

const SYSTEM_SKILL: &str = "markdown/skills/system.md";
const BUS: &str = "markdown/instances/system/escurel-event-bus.md";
const LEXWARE: &str = "markdown/instances/system/lexware.md";
const PLAIN: &str = "markdown/instances/system/plain.md";

fn system_skill() -> &'static str {
    "---\ntype: skill\nid: system\ndescription: An external system.\noptional_frontmatter: [kind, endpoint]\nlisting_sql: select id, kind from systems\n---\n# system\n\nGroup by `kind` in reports.\n"
}
fn bus() -> &'static str {
    "---\ntype: instance\nskill: system\nid: escurel-event-bus\nkind: bus\nendpoint: nats://x\n---\n# bus\n"
}
fn lexware() -> &'static str {
    "---\ntype: instance\nskill: system\nid: lexware\nkind: saas-api   # transport\nendpoint: https://api.lexware.io\n---\n# lexware\n"
}
fn plain() -> &'static str {
    "---\ntype: instance\nskill: system\nid: plain\n---\n# plain\n"
}

async fn seed(h: &Harness) {
    put(h, SYSTEM_SKILL, system_skill()).await;
    put(h, BUS, bus()).await;
    put(h, LEXWARE, lexware()).await;
    put(h, PLAIN, plain()).await;
}

#[tokio::test]
async fn without_the_option_the_colliding_pages_are_conflicts_as_before() {
    let h = fresh();
    seed(&h).await;

    let report = h.indexer.migrate_kind(false).await.unwrap();

    let mut conflicts = report.conflicts.clone();
    conflicts.sort();
    assert_eq!(conflicts, vec![BUS.to_owned(), LEXWARE.to_owned()]);
    assert!(report.fields_renamed.is_empty());
}

#[tokio::test]
async fn a_dry_run_with_the_option_reports_the_renames_and_writes_nothing() {
    let h = fresh();
    seed(&h).await;

    let report = h
        .indexer
        .migrate_kind_renaming(false, &renames())
        .await
        .unwrap();

    assert!(!report.applied);
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    let mut pages: Vec<&str> = report
        .fields_renamed
        .iter()
        .map(|f| f.page.as_str())
        .collect();
    pages.sort_unstable();
    assert_eq!(pages, vec![BUS, LEXWARE]);
    assert!(
        report
            .fields_renamed
            .iter()
            .all(|f| f.from == "kind" && f.to == "system_kind")
    );
    assert_eq!(report.skill_declarations_renamed.len(), 1);
    assert_eq!(report.skill_declarations_renamed[0].page, SYSTEM_SKILL);
    assert_eq!(lane(&h, BUS).await, bus(), "a dry run writes nothing");
    assert_eq!(lane(&h, SYSTEM_SKILL).await, system_skill());
}

#[tokio::test]
async fn apply_renames_values_preserved_rewrites_the_skill_and_a_second_run_is_a_no_op() {
    let h = fresh();
    seed(&h).await;

    let report = h
        .indexer
        .migrate_kind_renaming(true, &renames())
        .await
        .unwrap();

    assert!(report.applied);
    assert!(report.conflicts.is_empty());
    assert!(!report.tenant_quarantined, "everything legacy was migrated");
    assert_eq!(
        lane(&h, BUS).await,
        "---\nkind: instance\nskill: system\nid: escurel-event-bus\nsystem_kind: bus\nendpoint: nats://x\n---\n# bus\n"
    );
    assert_eq!(
        lane(&h, LEXWARE).await,
        "---\nkind: instance\nskill: system\nid: lexware\nsystem_kind: saas-api   # transport\nendpoint: https://api.lexware.io\n---\n# lexware\n"
    );
    let skill = lane(&h, SYSTEM_SKILL).await;
    assert!(skill.starts_with("---\nkind: skill\nid: system\n"));
    assert!(skill.contains("optional_frontmatter: [system_kind, endpoint]"));
    // The derived index sees the migrated pages: the domain field is `system_kind`, the page kind
    // is `kind: instance`.
    let items = h
        .indexer
        .list_instances("system", None, None, None, None, None)
        .await
        .unwrap();
    let lex = items
        .iter()
        .find(|i| i.frontmatter["id"] == "lexware")
        .unwrap();
    assert_eq!(lex.frontmatter["system_kind"], "saas-api");
    assert_eq!(lex.frontmatter["kind"], "instance");

    let again = h
        .indexer
        .migrate_kind_renaming(true, &renames())
        .await
        .unwrap();
    assert!(again.pages_to_migrate.is_empty(), "idempotent");
    assert!(again.fields_renamed.is_empty());
    assert!(again.skill_declarations_renamed.is_empty());
    assert_eq!(lane(&h, SYSTEM_SKILL).await, skill, "no churn on re-run");
}

#[tokio::test]
async fn a_page_that_already_has_the_new_name_is_a_collision_never_overwritten() {
    let h = fresh();
    seed(&h).await;
    let clash = "markdown/instances/system/clash.md";
    let clash_md = "---\ntype: instance\nskill: system\nid: clash\nkind: bus\nsystem_kind: other\n---\n# clash\n";
    put(&h, clash, clash_md).await;

    let report = h
        .indexer
        .migrate_kind_renaming(true, &renames())
        .await
        .unwrap();

    assert_eq!(report.rename_collisions, vec![clash.to_owned()]);
    assert!(report.conflicts.contains(&clash.to_owned()));
    assert_eq!(lane(&h, clash).await, clash_md, "left exactly as it was");
    assert!(
        report.tenant_quarantined,
        "the colliding page is still legacy"
    );
}

#[tokio::test]
async fn references_the_rename_does_not_rewrite_are_listed_as_warnings() {
    let h = fresh();
    seed(&h).await;

    let report = h
        .indexer
        .migrate_kind_renaming(false, &renames())
        .await
        .unwrap();

    let warnings = report.rename_warnings.join("\n");
    assert!(
        warnings.contains(&format!("{SYSTEM_SKILL}:6:")) && warnings.contains("listing_sql"),
        "{warnings}"
    );
    assert!(
        warnings.contains(&format!("{SYSTEM_SKILL}:10:")),
        "{warnings}"
    );
}

#[tokio::test]
async fn an_open_draft_of_a_colliding_page_and_its_snapshots_are_renamed_too() {
    let h = fresh();
    seed(&h).await;
    let proposed =
        "---\ntype: instance\nskill: system\nid: sap\nkind: erp\nendpoint: sap://x\n---\n# sap\n";
    let open = h
        .indexer
        .create_draft(NewDraft {
            target_page_id: "markdown/instances/system/sap.md".to_owned(),
            content: proposed.to_owned(),
            base_sha256: None,
            author: "agent:x".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();

    let dry = h
        .indexer
        .migrate_kind_renaming(false, &renames())
        .await
        .unwrap();
    assert_eq!(dry.drafts.len(), 1, "the draft would be rewritten");

    let report = h
        .indexer
        .migrate_kind_renaming(true, &renames())
        .await
        .unwrap();
    assert_eq!(report.drafts.len(), 1);
    let after = h.indexer.get_draft(&open.draft_id).await.unwrap().unwrap();
    assert_eq!(
        after.content,
        "---\nkind: instance\nskill: system\nid: sap\nsystem_kind: erp\nendpoint: sap://x\n---\n# sap\n"
    );
    assert_eq!(after.status, "open");
}
