//! Deterministic per-instance WRITE ACL (`Indexer::may_write_instance`).
//! Real DuckDB + FsStore, no mocks. Symmetric to `acl.rs` (reads) but
//! WITHOUT a public shortcut: a write needs admin, or ownership of BOTH
//! the existing page (no hijack) and the incoming content (no transfer);
//! public / no-`owner_field` instances are admin-write-only.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{AclCaller, Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use serde_json::Value;
use tempfile::TempDir;

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

const SKILL_MEMBER: (&str, &str) = (
    "markdown/skills/community_member.md",
    "---\ntype: skill\nid: community_member\ndescription: A member.\n\
     visibility: owner\nowner_field: credential\n---\n# community_member\n",
);
const SKILL_EVENT_PROFILE: (&str, &str) = (
    "markdown/skills/event_profile.md",
    "---\ntype: skill\nid: event_profile\ndescription: Per-event profile.\n\
     visibility: owner\nowner_field: member\n---\n# event_profile\n",
);
const SKILL_TALK: (&str, &str) = (
    "markdown/skills/talk.md",
    "---\ntype: skill\nid: talk\ndescription: A program item.\n\
     visibility: public\n---\n# talk\n",
);
const INST_ALICE: (&str, &str) = (
    "markdown/instances/community_member/alice.md",
    "---\ntype: instance\nskill: community_member\nid: alice\n\
     credential: \"whatsapp:111\"\n---\n# Alice\n",
);
const INST_ALICE_PROFILE: (&str, &str) = (
    "markdown/instances/event_profile/alice-ki-gipfel.md",
    "---\ntype: instance\nskill: event_profile\nid: alice-ki-gipfel\n\
     member: \"[[community_member::alice]]\"\nevent: ki-gipfel\n---\n# Alice @ KI-Gipfel\n",
);
const INST_TALK: (&str, &str) = (
    "markdown/instances/talk/keynote.md",
    "---\ntype: instance\nskill: talk\nid: keynote\nevent: ki-gipfel\n---\n# Keynote\n",
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

/// The frontmatter of one seeded instance, by skill + id.
async fn fm(h: &Harness, skill: &str, id: &str) -> Value {
    h.indexer
        .list_instances(skill, None, None, None, None, None)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.frontmatter.get("id").and_then(|v| v.as_str()) == Some(id))
        .unwrap_or_else(|| panic!("instance {skill}/{id} not seeded"))
        .frontmatter
}

fn member(subject: &str) -> AclCaller<'_> {
    AclCaller {
        subject,
        is_admin: false,
    }
}
fn admin<'a>() -> AclCaller<'a> {
    AclCaller {
        subject: "ops",
        is_admin: true,
    }
}

#[tokio::test]
async fn owner_overwrites_own_instance() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEMBER, INST_ALICE]).await;
    let alice = fm(&h, "community_member", "alice").await;
    assert!(
        h.indexer
            .may_write_instance(&member(ALICE), "community_member", Some(&alice), &alice)
            .await
            .unwrap(),
        "alice may overwrite her own record"
    );
}

#[tokio::test]
async fn non_owner_overwrite_denied() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEMBER, INST_ALICE]).await;
    let alice = fm(&h, "community_member", "alice").await;
    assert!(
        !h.indexer
            .may_write_instance(&member(BOB), "community_member", Some(&alice), &alice)
            .await
            .unwrap(),
        "bob must NOT overwrite alice's record (no hijack)"
    );
}

#[tokio::test]
async fn create_for_another_owner_denied() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEMBER, INST_ALICE]).await;
    let alice = fm(&h, "community_member", "alice").await;
    // No existing page (a create), but the incoming owner is alice.
    assert!(
        !h.indexer
            .may_write_instance(&member(BOB), "community_member", None, &alice)
            .await
            .unwrap(),
        "bob must NOT create a record owned by alice (no transfer)"
    );
}

#[tokio::test]
async fn public_instance_is_admin_write_only() {
    let h = fresh_harness();
    seed(&h, &[SKILL_TALK, INST_TALK]).await;
    let talk = fm(&h, "talk", "keynote").await;
    assert!(
        !h.indexer
            .may_write_instance(&member(BOB), "talk", Some(&talk), &talk)
            .await
            .unwrap(),
        "a public talk has no owner → non-admin write denied"
    );
    assert!(
        h.indexer
            .may_write_instance(&admin(), "talk", Some(&talk), &talk)
            .await
            .unwrap(),
        "admin may curate public talks"
    );
}

#[tokio::test]
async fn admin_writes_any_owner_private_instance() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEMBER, INST_ALICE]).await;
    let alice = fm(&h, "community_member", "alice").await;
    assert!(
        h.indexer
            .may_write_instance(&admin(), "community_member", Some(&alice), &alice)
            .await
            .unwrap(),
        "admin bypasses owner-write"
    );
}

#[tokio::test]
async fn owner_may_tombstone_own_instance() {
    // Self-deletion (`/delete-my-data`) repoints the owner wikilink at a
    // deleted placeholder (unresolvable). The OWNER of the existing page may
    // still write it — owning the existing page authorises the write, not the
    // incoming owner. (Would be denied if the incoming owner were re-checked.)
    let h = fresh_harness();
    seed(
        &h,
        &[
            SKILL_MEMBER,
            SKILL_EVENT_PROFILE,
            INST_ALICE,
            INST_ALICE_PROFILE,
        ],
    )
    .await;
    let existing = fm(&h, "event_profile", "alice-ki-gipfel").await;
    let tombstone = serde_json::json!({
        "type": "instance",
        "skill": "event_profile",
        "id": "alice-ki-gipfel",
        "member": "[[community_member::geloescht]]",
        "event": "ki-gipfel",
    });
    assert!(
        h.indexer
            .may_write_instance(&member(ALICE), "event_profile", Some(&existing), &tombstone)
            .await
            .unwrap(),
        "owner may tombstone/release their own record"
    );
    assert!(
        !h.indexer
            .may_write_instance(&member(BOB), "event_profile", Some(&existing), &tombstone)
            .await
            .unwrap(),
        "a non-owner still cannot (existing owner is alice)"
    );
}

#[tokio::test]
async fn owner_resolved_through_wikilink_for_write() {
    let h = fresh_harness();
    seed(
        &h,
        &[
            SKILL_MEMBER,
            SKILL_EVENT_PROFILE,
            INST_ALICE,
            INST_ALICE_PROFILE,
        ],
    )
    .await;
    let profile = fm(&h, "event_profile", "alice-ki-gipfel").await;
    assert!(
        h.indexer
            .may_write_instance(&member(ALICE), "event_profile", Some(&profile), &profile)
            .await
            .unwrap(),
        "alice owns the event_profile via member→credential wikilink"
    );
    assert!(
        !h.indexer
            .may_write_instance(&member(BOB), "event_profile", Some(&profile), &profile)
            .await
            .unwrap(),
        "bob must NOT write alice's event_profile"
    );
}
