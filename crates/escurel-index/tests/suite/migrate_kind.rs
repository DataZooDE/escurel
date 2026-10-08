//! `Indexer::migrate_kind`: the one-way rewrite of the page-kind key `type:` -> `kind:`.
//!
//! Real DuckDB + real file store + real Loro snapshots, no mocks. The failure modes this guards
//! are the ones that lose data or take a tenant down: a dry run that writes, a rewrite that
//! touches a user's own `type:` field, an open draft left unpromotable, a signed pack page
//! rewritten under its signature, a run that is not idempotent.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::drafts::{NewDraft, content_hash};
use escurel_index::{AclCaller, Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    /// A second connection onto the SAME database instance the indexer uses (a fresh
    /// `Connection::open` would be a separate instance whose writes the indexer cannot see).
    side: Connection,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let db_path = db_dir.path().join("escurel.duckdb");
    let conn = Connection::open(&db_path).unwrap();
    Migrator::up(&conn).unwrap();
    let side = conn.try_clone().unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        store,
        indexer,
        side,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

fn key(path: &str) -> Key {
    Key::new(TENANT, path.to_owned()).unwrap()
}

/// Write a page straight into the lane, the way an OLD store holds it. After the hard cut a
/// legacy page can no longer go through `update_page`; the lane is where it lives. Pages that
/// already use `kind:` are indexed too, as they would be in a half-migrated tenant.
async fn put(h: &Harness, path: &str, md: &str) {
    h.store
        .write(&key(path), Bytes::from(md.to_owned()))
        .await
        .unwrap();
    if escurel_md::parse(md).is_ok() {
        h.indexer.update_page(path, md).await.unwrap();
    }
}

async fn lane(h: &Harness, path: &str) -> String {
    String::from_utf8(h.store.read(&key(path)).await.unwrap().to_vec()).unwrap()
}

const SKILL: &str = "markdown/skills/customer.md";
const C1: &str = "markdown/instances/customer/c1.md";
const INVOICE: &str = "markdown/instances/doc/inv1.md";
const BOTH: &str = "markdown/instances/doc/both.md";
const BASE: &str = "markdown/base/pack/skills/shared.md";

fn skill_md() -> &'static str {
    "---\ntype: skill\nid: customer\ndescription: A buyer.\n---\n# customer\n"
}
fn c1_md() -> &'static str {
    "---\ntype: instance\nskill: customer\nid: c1\n---\n# c1\n\ntype: instance in prose stays.\n"
}
fn invoice_md() -> &'static str {
    // An instance that is ALREADY on `kind:` and has its own data field named `type`.
    "---\nkind: instance\nskill: doc\nid: inv1\ntype: invoice\n---\n# inv1\n"
}
fn both_md() -> &'static str {
    "---\ntype: instance\nkind: instance\nskill: doc\nid: both\n---\n# both\n"
}
fn base_md() -> &'static str {
    "---\ntype: skill\nid: shared\n---\n# shared\n"
}

async fn seed(h: &Harness) {
    put(h, SKILL, skill_md()).await;
    put(h, C1, c1_md()).await;
    put(h, INVOICE, invoice_md()).await;
    put(h, BOTH, both_md()).await;
    put(h, BASE, base_md()).await;
}

#[tokio::test]
async fn a_dry_run_reports_what_it_would_do_and_writes_nothing() {
    let h = fresh();
    seed(&h).await;

    let report = h.indexer.migrate_kind(false).await.unwrap();

    assert!(!report.applied);
    let mut would = report.pages_to_migrate.clone();
    would.sort();
    assert_eq!(would, vec![C1.to_owned(), SKILL.to_owned()]);
    assert_eq!(report.conflicts, vec![BOTH.to_owned()]);
    assert_eq!(report.skipped_pack_base, vec![BASE.to_owned()]);
    assert_eq!(
        report.already_kind, 1,
        "the invoice page is already on kind:"
    );
    // Nothing was written: every lane object is byte-identical.
    assert_eq!(lane(&h, SKILL).await, skill_md());
    assert_eq!(lane(&h, C1).await, c1_md());
    assert!(
        report.audit_event_id.is_none(),
        "a dry run records no audit event"
    );
}

#[tokio::test]
async fn apply_rewrites_only_the_page_kind_key_and_a_second_run_is_a_no_op() {
    let h = fresh();
    seed(&h).await;

    let report = h.indexer.migrate_kind(true).await.unwrap();
    assert!(report.applied);
    assert_eq!(report.pages_to_migrate.len(), 2);

    assert_eq!(
        lane(&h, SKILL).await,
        "---\nkind: skill\nid: customer\ndescription: A buyer.\n---\n# customer\n"
    );
    assert_eq!(
        lane(&h, C1).await,
        "---\nkind: instance\nskill: customer\nid: c1\n---\n# c1\n\ntype: instance in prose stays.\n",
        "prose that happens to say `type: instance` is not frontmatter"
    );
    // The user's own `type: invoice` field is untouched, and so is everything else about the page.
    assert_eq!(lane(&h, INVOICE).await, invoice_md());
    // A conflict is reported and left exactly as it was; so is the signed pack page.
    assert_eq!(lane(&h, BOTH).await, both_md());
    assert_eq!(lane(&h, BASE).await, base_md());

    // The index follows the lane: the migrated page still reads back.
    let page = h
        .indexer
        .expand(C1, None, None)
        .await
        .unwrap()
        .expect("c1 is indexed");
    assert_eq!(page.page.skill, "customer");

    let again = h.indexer.migrate_kind(true).await.unwrap();
    assert!(
        again.pages_to_migrate.is_empty(),
        "idempotent: nothing left to migrate"
    );
    assert_eq!(again.already_kind, 3, "skill, c1 and the invoice page");
    assert_eq!(again.conflicts, vec![BOTH.to_owned()]);
    assert_eq!(lane(&h, SKILL).await, lane(&h, SKILL).await);
}

#[tokio::test]
async fn apply_preserves_who_wrote_each_page() {
    let h = fresh();
    // alice wrote the page when it was current (so the index knows who), then the lane came to
    // hold the old spelling: an old store.
    let current = c1_md().replace("type: instance", "kind: instance");
    h.indexer
        .update_page_as(C1, &current, Some("agent:alice"))
        .await
        .unwrap();
    h.store
        .write(&key(C1), Bytes::from(c1_md().to_owned()))
        .await
        .unwrap();

    h.indexer.migrate_kind(true).await.unwrap();

    assert!(
        lane(&h, C1).await.starts_with("---\nkind: instance\n"),
        "it was migrated"
    );
    let page = h.indexer.expand(C1, None, None).await.unwrap().unwrap();
    assert_eq!(page.last_written_by.as_deref(), Some("agent:alice"));
}

#[tokio::test]
async fn migrate_legacy_evolve_evidence_keeps_owner_acl_and_exact_bindings() {
    let h = fresh();
    let skill = "---\nkind: skill\nid: evolve_validation_report\ndescription: Private validation evidence.\nowner_field: owner_subject\nacl:\n  read: [owner]\n---\n# report\n";
    let policy_skill = "---\nkind: skill\nid: plan_policy\ndescription: Inactive candidate.\nowner_field: owner_subject\nacl:\n  read: [owner]\n---\n# policy\n";
    put(&h, "markdown/skills/evolve_validation_report.md", skill).await;
    put(&h, "markdown/skills/plan_policy.md", policy_skill).await;
    let report_path = "markdown/instances/evolve_validation_report/ep_123.md";
    let policy_path = "markdown/instances/plan_policy/ep_123.md";
    let report = "---\ntype: instance\nskill: evolve_validation_report\nid: ep_123\nowner_subject: alice\nacl:\n  read: [owner]\nwinner_sql_sha256: abc123\nreport_sha256: def456\n---\n# Sealed synthetic report\n";
    let policy = "---\ntype: instance\nskill: plan_policy\nid: ep_123\nowner_subject: alice\nacl:\n  read: [owner]\nstatus: synthetic_sandbox_candidate\nwinner_sql_sha256: abc123\nreport_sha256: def456\n---\n# Inactive candidate\n";
    put(&h, report_path, report).await;
    put(&h, policy_path, policy).await;

    let migrated = h.indexer.migrate_kind(true).await.unwrap();
    assert!(migrated.pages_to_migrate.contains(&report_path.to_owned()));
    assert!(migrated.pages_to_migrate.contains(&policy_path.to_owned()));
    for (path, before, skill) in [
        (report_path, report, "evolve_validation_report"),
        (policy_path, policy, "plan_policy"),
    ] {
        assert_eq!(
            lane(&h, path).await,
            before.replacen("type: instance", "kind: instance", 1)
        );
        let page = h.indexer.expand(path, None, None).await.unwrap().unwrap();
        assert_eq!(page.page.page_kind, escurel_md::PageKind::Instance);
        let frontmatter = h
            .indexer
            .list_instances(skill, None, None, None, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|item| item.frontmatter["id"] == "ep_123")
            .unwrap()
            .frontmatter;
        assert_eq!(frontmatter["owner_subject"], "alice");
        assert_eq!(frontmatter["winner_sql_sha256"], "abc123");
        assert_eq!(frontmatter["report_sha256"], "def456");
        assert!(
            h.indexer
                .may_read_instance(
                    &AclCaller {
                        subject: "alice",
                        is_admin: false,
                        token_groups: &[],
                        actor: None,
                        run_id: None,
                        root_event_id: None,
                        agent_skill: None
                    },
                    skill,
                    &frontmatter
                )
                .await
                .unwrap()
        );
        assert!(
            !h.indexer
                .may_read_instance(
                    &AclCaller {
                        subject: "bob",
                        is_admin: false,
                        token_groups: &[],
                        actor: None,
                        run_id: None,
                        root_event_id: None,
                        agent_skill: None
                    },
                    skill,
                    &frontmatter
                )
                .await
                .unwrap()
        );
    }
}

#[tokio::test]
async fn an_open_draft_is_rewritten_in_place_with_a_new_hash_and_decided_drafts_are_not() {
    let h = fresh();
    put(&h, SKILL, skill_md()).await;
    let proposed = "---\ntype: instance\nskill: customer\nid: c2\n---\n# c2\n";
    let open = h
        .indexer
        .create_draft(NewDraft {
            target_page_id: "markdown/instances/customer/c2.md".to_owned(),
            content: proposed.to_owned(),
            base_sha256: None,
            author: "agent:x".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    let decided = h
        .indexer
        .create_draft(NewDraft {
            target_page_id: "markdown/instances/customer/c3.md".to_owned(),
            content: "---\ntype: instance\nskill: customer\nid: c3\n---\n# c3\n".to_owned(),
            base_sha256: None,
            author: "agent:x".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    h.indexer
        .close_draft(&decided.draft_id, "discarded", "alice", "no")
        .await
        .unwrap();

    let report = h.indexer.migrate_kind(true).await.unwrap();

    assert_eq!(report.drafts.len(), 1);
    let m = &report.drafts[0];
    assert_eq!(m.draft_id, open.draft_id);
    assert_eq!(m.old_sha256, content_hash(proposed));
    let after = h.indexer.get_draft(&open.draft_id).await.unwrap().unwrap();
    assert!(after.content.starts_with("---\nkind: instance\n"));
    assert_eq!(after.content_sha256, content_hash(&after.content));
    assert_eq!(after.content_sha256, m.new_sha256);
    assert_ne!(after.content_sha256, open.content_sha256);
    assert_eq!(after.status, "open");
    // The decided draft keeps its bytes: it is history, not pending work.
    let kept = h
        .indexer
        .get_draft(&decided.draft_id)
        .await
        .unwrap()
        .unwrap();
    assert!(kept.content.starts_with("---\ntype: instance\n"));
    // The audit trail records that the bytes changed and why.
    let id = report
        .audit_event_id
        .expect("an applied migration records an audit event");
    let event = h
        .indexer
        .get_event(&id)
        .await
        .unwrap()
        .expect("event stored");
    assert_eq!(event.label_skill, "escurel:kind-migration");
    assert!(event.body.contains(&open.draft_id), "{}", event.body);
}

#[tokio::test]
async fn historical_crdt_snapshots_are_rewritten_so_history_still_reads() {
    let h = fresh();
    put(&h, SKILL, skill_md()).await;
    let page = "markdown/instances/engagement/spine.md";
    let old = "---\ntype: instance\nskill: engagement\nid: spine\nat: 2026-03-01T00:00:00Z\nphase: a\n---\n# Spine\n";
    put(&h, page, old).await;
    h.indexer
        .seed_snapshot_history(page, &[("2026-03-10T00:00:00Z", old)])
        .await
        .unwrap();

    let dry = h.indexer.migrate_kind(false).await.unwrap();
    assert_eq!(dry.snapshots_rewritten, 0, "a dry run rewrites no snapshot");
    assert_eq!(
        dry.snapshots_to_rewrite, 1,
        "but it counts what it would rewrite"
    );

    let report = h.indexer.migrate_kind(true).await.unwrap();
    assert_eq!(report.snapshots_rewritten, 1);

    // Snapshot bytes are Loro; the table is read directly, the way history reads it.
    let bytes: Vec<u8> = h
        .side
        .query_row(
            "SELECT snapshot_bytes FROM crdt_snapshots WHERE page_id = ?",
            [page],
            |r| r.get(0),
        )
        .unwrap();
    let md = escurel_crdt::body_from_snapshot(&bytes).unwrap();
    assert!(md.starts_with("---\nkind: instance\n"), "{md}");
    assert!(
        !md.contains("type: instance"),
        "no legacy key left in history: {md}"
    );
}

#[tokio::test]
async fn apply_refuses_while_a_page_has_crdt_ops_newer_than_its_newest_snapshot() {
    let h = fresh();
    let page = "markdown/instances/engagement/live.md";
    let old = "---\ntype: instance\nskill: engagement\nid: live\n---\n# Live\n";
    put(&h, page, old).await;
    h.indexer
        .seed_snapshot_history(page, &[("2026-03-10T00:00:00Z", old)])
        .await
        .unwrap();
    {
        let conn = &h.side;
        conn.execute(
            "INSERT INTO crdt_ops (page_id, op_id, hlc, op_bytes) VALUES (?, 'o1', 99, ?)",
            duckdb::params![page, vec![1u8, 2, 3]],
        )
        .unwrap();
    }

    let dry = h.indexer.migrate_kind(false).await.unwrap();
    assert_eq!(dry.crdt_pages_with_live_ops, vec![page.to_owned()]);

    let err = h
        .indexer
        .migrate_kind(true)
        .await
        .expect_err("apply must refuse");
    assert!(err.to_string().contains("live"), "{err}");
    // Nothing was written by the refused run.
    assert_eq!(lane(&h, page).await, old);
}

const RUN_PAGE: &str = "markdown/instances/workflow-run/r1.md";
const LEAD_PAGE: &str = "markdown/instances/lead/l1.md";

#[tokio::test]
async fn a_workflow_run_boards_status_is_renamed_run_status_and_a_tenants_status_is_not() {
    let h = fresh();
    // A run board from before the rename: legacy page kind AND the old `status:` key.
    put(
        &h,
        RUN_PAGE,
        "---\ntype: instance\nskill: workflow-run\nid: r1\nwf_skill: deep-research\nstatus: stopped\n---\n# run\n",
    )
    .await;
    // A board that already uses `kind:` but still the old status key.
    put(
        &h,
        "markdown/instances/workflow-run/r2.md",
        "---\nkind: instance\nskill: workflow-run\nid: r2\nstatus: running\n---\n# run\n",
    )
    .await;
    // A tenant's own `status` data must never be renamed.
    let lead = "---\nkind: instance\nskill: lead\nid: l1\nstatus: qualified\n---\n# l1\n";
    put(&h, LEAD_PAGE, lead).await;

    let dry = h.indexer.migrate_kind(false).await.unwrap();
    let mut would = dry.run_status_renamed.clone();
    would.sort();
    assert_eq!(
        would,
        vec![
            RUN_PAGE.to_owned(),
            "markdown/instances/workflow-run/r2.md".to_owned()
        ]
    );
    assert!(
        lane(&h, RUN_PAGE).await.contains("\nstatus: stopped\n"),
        "a dry run writes nothing"
    );

    h.indexer.migrate_kind(true).await.unwrap();

    assert_eq!(
        lane(&h, RUN_PAGE).await,
        "---\nkind: instance\nskill: workflow-run\nid: r1\nwf_skill: deep-research\nrun_status: stopped\n---\n# run\n"
    );
    assert!(
        lane(&h, "markdown/instances/workflow-run/r2.md")
            .await
            .contains("\nrun_status: running\n")
    );
    assert_eq!(
        lane(&h, LEAD_PAGE).await,
        lead,
        "tenant status data untouched"
    );
    let again = h.indexer.migrate_kind(true).await.unwrap();
    assert!(again.run_status_renamed.is_empty(), "idempotent");
    assert!(again.pages_to_migrate.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_historical_snapshots_are_rewritten_in_one_apply() {
    // A real tenant has hundreds of pages with snapshot history; the migration must walk them all and
    // finish (one snapshot, as above, would not exercise the per-snapshot connection hand-off).
    let h = fresh();
    put(&h, SKILL, skill_md()).await;
    let n = 60;
    for i in 0..n {
        let page = format!("markdown/instances/engagement/e{i}.md");
        let old = format!(
            "---\ntype: instance\nskill: engagement\nid: e{i}\nat: 2026-03-01T00:00:00Z\nphase: a\n---\n# E{i}\n"
        );
        put(&h, &page, &old).await;
        h.indexer
            .seed_snapshot_history(&page, &[("2026-03-10T00:00:00Z", &old)])
            .await
            .unwrap();
    }

    // Booted the way the server boots a legacy tenant: quarantined, so the pages are rewritten in the
    // lane only and the whole index is REBUILT at the end (with snapshot history present).
    assert!(h.indexer.quarantine_legacy_kind_pages().await.unwrap());
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        h.indexer.migrate_kind(true),
    )
    .await
    .expect("the migration hung")
    .unwrap();
    assert_eq!(report.snapshots_rewritten, n);
    let left: i64 = h
        .side
        .query_row("SELECT count(*) FROM crdt_snapshots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, n as i64);
}
