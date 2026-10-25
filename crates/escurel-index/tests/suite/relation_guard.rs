//! `Indexer::relation_referrers`: the in-use guard for declared relations.
//! Real DuckDB, real FsStore, neutral skills.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const EPIC: &str = "markdown/instances/epic/launch.md";

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

async fn put(h: &Harness, page: &str, body: &str) {
    let key = Key::new(TENANT, page.to_owned()).unwrap();
    h.store
        .write(&key, Bytes::from(body.to_owned()))
        .await
        .unwrap();
    h.indexer.update_page(page, body).await.unwrap();
}

fn epic_skill(extra: &str) -> String {
    format!("---\nkind: skill\nid: epic\ndescription: A group.\n{extra}---\n# epic\n")
}

const TICKET_SKILL: &str = "---\nkind: skill\nid: ticket\ndescription: A work item.\nfields:\n  - {name: epic, kind: link, to: epic}\n  - {name: blocks, kind: link, to: ticket}\n---\n# ticket\n";

fn ticket(id: &str, fm: &str) -> String {
    format!("---\nkind: instance\nskill: ticket\nid: {id}\n{fm}---\n# {id}\n")
}

async fn seeded(epic_extra: &str) -> Harness {
    let h = harness();
    put(&h, "markdown/skills/epic.md", &epic_skill(epic_extra)).await;
    put(&h, "markdown/skills/ticket.md", TICKET_SKILL).await;
    put(
        &h,
        EPIC,
        "---\nkind: instance\nskill: epic\nid: launch\n---\n# launch\n",
    )
    .await;
    h
}

#[tokio::test]
async fn a_declared_link_holds_the_target_in_use() {
    let h = seeded("").await;
    put(
        &h,
        "markdown/instances/ticket/t1.md",
        &ticket("t1", "epic: \"[[epic::launch]]\"\n"),
    )
    .await;
    put(&h, "markdown/instances/ticket/t2.md", &ticket("t2", "")).await;
    let r = h
        .indexer
        .relation_referrers(EPIC, &HashMap::new())
        .await
        .unwrap();
    assert_eq!(r.total, 1, "{r:?}");
    assert_eq!(r.items[0].page_id, "markdown/instances/ticket/t1.md");
    assert_eq!(r.items[0].field, "epic");
    let issue = r.issue(EPIC, "archive").unwrap();
    assert_eq!(issue.code, "relation_in_use");
}

#[tokio::test]
async fn an_undeclared_wikilink_is_not_guarded() {
    let h = seeded("").await;
    put(
        &h,
        "markdown/instances/ticket/t1.md",
        "---\nkind: instance\nskill: ticket\nid: t1\nnote: \"[[epic::launch]]\"\n---\n# t1\nSee [[epic::launch]].\n",
    )
    .await;
    let r = h
        .indexer
        .relation_referrers(EPIC, &HashMap::new())
        .await
        .unwrap();
    assert_eq!(r.total, 0, "{r:?}");
}

#[tokio::test]
async fn the_target_skill_can_opt_out() {
    let h = seeded("relation_guard: false\n").await;
    put(
        &h,
        "markdown/instances/ticket/t1.md",
        &ticket("t1", "epic: \"[[epic::launch]]\"\n"),
    )
    .await;
    let r = h
        .indexer
        .relation_referrers(EPIC, &HashMap::new())
        .await
        .unwrap();
    assert_eq!(r.total, 0, "{r:?}");
}

#[tokio::test]
async fn a_malformed_flag_is_reported_on_the_skill() {
    let h = harness();
    let issues = h
        .indexer
        .validate(None, &epic_skill("relation_guard: nope\n"))
        .await
        .unwrap();
    assert!(
        issues.iter().any(|i| i.code == "skill_rule_malformed"),
        "{issues:?}"
    );
}

#[tokio::test]
async fn the_list_is_capped_and_counts_everything() {
    let h = seeded("").await;
    for n in 0..25 {
        let id = format!("t{n:02}");
        put(
            &h,
            &format!("markdown/instances/ticket/{id}.md"),
            &ticket(&id, "epic: \"[[epic::launch]]\"\n"),
        )
        .await;
    }
    let r = h
        .indexer
        .relation_referrers(EPIC, &HashMap::new())
        .await
        .unwrap();
    assert_eq!(r.total, 25);
    assert_eq!(r.items.len(), escurel_index::RELATION_REFERRER_LIMIT);
    assert!(r.issue(EPIC, "archive").unwrap().message.contains("5 more"));
}

#[tokio::test]
async fn a_referrer_the_batch_rewrites_or_archives_does_not_block() {
    let h = seeded("").await;
    let t1 = "markdown/instances/ticket/t1.md";
    put(&h, t1, &ticket("t1", "epic: \"[[epic::launch]]\"\n")).await;
    // Re-pointed in the batch.
    let repointed = HashMap::from([(t1.to_owned(), ticket("t1", ""))]);
    assert_eq!(
        h.indexer
            .relation_referrers(EPIC, &repointed)
            .await
            .unwrap()
            .total,
        0
    );
    // Archived in the batch.
    let archived = HashMap::from([(
        t1.to_owned(),
        ticket("t1", "archived: true\nepic: \"[[epic::launch]]\"\n"),
    )]);
    assert_eq!(
        h.indexer
            .relation_referrers(EPIC, &archived)
            .await
            .unwrap()
            .total,
        0
    );
    // A referrer the batch creates does block.
    let created = HashMap::from([(
        "markdown/instances/ticket/t9.md".to_owned(),
        ticket("t9", "epic: \"[[epic::launch]]\"\n"),
    )]);
    assert_eq!(
        h.indexer
            .relation_referrers(EPIC, &created)
            .await
            .unwrap()
            .total,
        2
    );
}

#[tokio::test]
async fn every_listed_target_skill_is_guarded() {
    let h = harness();
    put(&h, "markdown/skills/epic.md", &epic_skill("")).await;
    put(
        &h,
        "markdown/skills/story.md",
        "---\nkind: skill\nid: story\ndescription: s\n---\n# story\n",
    )
    .await;
    put(
        &h,
        "markdown/skills/ticket.md",
        "---\nkind: skill\nid: ticket\ndescription: t\nfields:\n  - {name: parent, kind: link, to: [epic, story]}\n---\n# ticket\n",
    )
    .await;
    put(
        &h,
        EPIC,
        "---\nkind: instance\nskill: epic\nid: launch\n---\n# launch\n",
    )
    .await;
    let story = "markdown/instances/story/s1.md";
    put(
        &h,
        story,
        "---\nkind: instance\nskill: story\nid: s1\n---\n# s1\n",
    )
    .await;
    put(
        &h,
        "markdown/instances/ticket/t1.md",
        &ticket("t1", "parent: \"[[story::s1]]\"\n"),
    )
    .await;
    for target in [story, EPIC] {
        let r = h
            .indexer
            .relation_referrers(target, &HashMap::new())
            .await
            .unwrap();
        assert_eq!(r.total, usize::from(target == story), "{target}: {r:?}");
    }
}
