//! Deterministic per-instance ACL (`Indexer::may_read_instance`). Real
//! DuckDB + FsStore, no mocks. Covers direct-field ownership
//! (`community_member.credential`), wikilink indirection
//! (`event_profile.member → community_member.credential`), public skills,
//! and the admin bypass.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{AclCaller, Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
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
const INST_BOB: (&str, &str) = (
    "markdown/instances/community_member/bob.md",
    "---\ntype: instance\nskill: community_member\nid: bob\n\
     credential: \"whatsapp:222\"\n---\n# Bob\n",
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
async fn fm(h: &Harness, skill: &str, id: &str) -> serde_json::Value {
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

#[tokio::test]
async fn owner_reads_own_record_direct_credential() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEMBER, INST_ALICE]).await;
    let alice = fm(&h, "community_member", "alice").await;

    assert!(
        h.indexer
            .may_read_instance(&member(ALICE), "community_member", &alice)
            .await
            .unwrap(),
        "alice may read her own community_member profile"
    );
}

#[tokio::test]
async fn non_owner_denied_owner_visibility_instance() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEMBER, INST_ALICE, INST_BOB]).await;
    let alice = fm(&h, "community_member", "alice").await;

    assert!(
        !h.indexer
            .may_read_instance(&member(BOB), "community_member", &alice)
            .await
            .unwrap(),
        "bob must NOT read alice's owner-private profile"
    );
}

#[tokio::test]
async fn owner_resolved_through_member_wikilink() {
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
            .may_read_instance(&member(ALICE), "event_profile", &profile)
            .await
            .unwrap(),
        "alice owns the event_profile via member → community_member → credential"
    );
    assert!(
        !h.indexer
            .may_read_instance(&member(BOB), "event_profile", &profile)
            .await
            .unwrap(),
        "bob must NOT read alice's event_profile"
    );
}

#[tokio::test]
async fn public_instance_readable_by_anyone() {
    let h = fresh_harness();
    seed(&h, &[SKILL_TALK, INST_TALK]).await;
    let talk = fm(&h, "talk", "keynote").await;

    assert!(
        h.indexer
            .may_read_instance(&member(BOB), "talk", &talk)
            .await
            .unwrap(),
        "a public talk is readable by any caller"
    );
}

#[tokio::test]
async fn admin_bypasses_owner_visibility() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEMBER, INST_ALICE]).await;
    let alice = fm(&h, "community_member", "alice").await;

    let admin = AclCaller {
        subject: "operator",
        is_admin: true,
    };
    assert!(
        h.indexer
            .may_read_instance(&admin, "community_member", &alice)
            .await
            .unwrap(),
        "the admin role bypasses owner-visibility (operator dashboard)"
    );
}
