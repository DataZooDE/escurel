//! The skill-page frontmatter keys that carry AUTHORITY: who may read or
//! write (`acl`, `visibility`, `owner_field`, `instance_acl`), who a run
//! executes as (`agent`), whether its writes land unreviewed (`autonomy`),
//! when and where it runs (`schedule`, `schedule_budget`, `schedule_webhook`, `requires`,
//! `harness`, `model`), what it triggers (`cascade`, the `kind: event`
//! entries of `actions`, a `backend` of `kind: workflow`), what it connects
//! to (`backend`, `chat`) and how it is governed (`promotable`, `protected`,
//! `immutable`, `freeze_when` — deleting the last would unfreeze every
//! instance it holds).
//!
//! Not listed, on purpose: `layer` has its own guard (a base-layer page is
//! read-only and `update_page` may not fabricate one), and the descriptive
//! keys (`description`, `summary`, `fields`, `required_frontmatter`, `viewer`,
//! `stale_after`, …) shape or present data without deciding who may act or
//! what runs.
//!
//! One list, read by two consumers that must agree: the gateway's write gate
//! (a change to any of these on a skill page needs an admin or the stored
//! page's `acl.update`) and the SKILL.md importer, which names them to the
//! curator before a third-party bundle is promoted. `description`,
//! `summary`, `fields` and the body stay free to edit.

/// The authority keys, in the order the importer reports them.
pub const AUTHORITY_KEYS: &[&str] = &[
    "acl",
    "visibility",
    "owner_field",
    "instance_acl",
    "agent",
    "autonomy",
    "schedule",
    "schedule_budget",
    "schedule_webhook",
    "requires",
    "harness",
    "model",
    "cascade",
    "actions",
    "chat",
    "backend",
    "promotable",
    "protected",
    "immutable",
    "freeze_when",
];

/// Whether `key` is one of the [`AUTHORITY_KEYS`].
#[must_use]
pub fn is_authority_key(key: &str) -> bool {
    AUTHORITY_KEYS.contains(&key)
}
