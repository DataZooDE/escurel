//! Deterministic per-instance access control.
//!
//! A `type: skill` page declares a read policy (`visibility: public|owner`,
//! [`crate::Visibility`]) and, for `owner` visibility, the frontmatter
//! field naming the owning principal (`owner_field:`). The check here is a
//! pure comparison on the read path — resolve the instance's owner from its
//! frontmatter, compare to the verified caller subject, allow on equality
//! or the admin role, fail closed otherwise. **No LLM, no agent, no
//! probabilistic classifier is ever in the authorisation decision.**
//!
//! Owner resolution handles one level of indirection: a direct field value
//! (e.g. `community_member.credential` → the platform `sub`) is the owner;
//! a `[[skill::id]]` wikilink (e.g. `event_profile.member`) is resolved to
//! the linked instance's `credential`.

use std::collections::HashSet;

use escurel_md::{PageType, YamlValue, parse};
use serde_json::Value;

use crate::meta_skill::META_SKILL_PAGE_ID;
use crate::read::AclPolicy;
use crate::{Indexer, IndexerError};

/// The verified caller, for an instance ACL decision. `subject` is the
/// token `sub` claim; `is_admin` is the admin-role bypass; `token_groups`
/// are the group names projected from the JWT `groups_claim` (already
/// admin-value-stripped by the server boundary — reserved-name stripping
/// still happens here, as defence in depth).
#[derive(Debug, Clone, Copy)]
pub struct AclCaller<'a> {
    pub subject: &'a str,
    pub is_admin: bool,
    pub token_groups: &'a [String],
}

/// The frontmatter field that carries a member's owning principal when an
/// owner is reached through a `[[community_member::id]]` wikilink.
const OWNER_CREDENTIAL_FIELD: &str = "credential";

/// The skill whose instance (id == `chat_group_id`) names the owner of a
/// chat group (ADR-13: `chat_group_id` := `community_member_id`). The
/// chat's owner is that instance's `owner_field` value, resolved the same
/// way as an instance owner.
const CHAT_OWNER_SKILL: &str = "community_member";

/// Group names that may be granted only *structurally* — never via a
/// token claim or a DuckDB membership row. `public` is always present for
/// an authenticated caller; `owner` only when the caller's `sub` resolves
/// to the instance owner; `admin` only via the verified admin role. Any
/// occurrence of these in a token/membership source is discarded so a
/// misconfigured IdP can never impersonate them (CR §8.2).
const RESERVED_GROUPS: [&str; 3] = ["public", "owner", "admin"];

/// The four CRUD verbs an `acl:` block grants. `Delete` is parsed and
/// reported but, in v1, deletion flows through the same overwrite path as
/// `Update` (there is no distinct delete operation at the write boundary);
/// it is enforced as `Update`. A distinct delete-verb gate is phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Read,
    Create,
    Update,
}

fn policy_verb(p: &AclPolicy, verb: Verb) -> Option<&Vec<String>> {
    match verb {
        Verb::Read => p.read.as_ref(),
        Verb::Create => p.create.as_ref(),
        Verb::Update => p.update.as_ref(),
    }
}

/// The shipped tenant default — reproduces today's behaviour (open read,
/// admin-only writes) for any tenant that has not configured
/// `acl_defaults` on its `escurel` meta-skill page.
fn shipped_tenant_default() -> AclPolicy {
    AclPolicy {
        read: Some(vec!["public".to_owned()]),
        create: Some(vec!["admin".to_owned()]),
        update: Some(vec!["admin".to_owned()]),
        delete: Some(vec!["admin".to_owned()]),
    }
}

impl Indexer {
    /// Whether `caller` may read an instance of `skill` with frontmatter
    /// `fm`. Deterministic: admin and public always pass; an owner-private
    /// instance passes only for its resolved owning subject; an
    /// owner-private instance whose owner cannot be resolved fails closed.
    pub async fn may_read_instance(
        &self,
        caller: &AclCaller<'_>,
        skill: &str,
        fm: &Value,
    ) -> Result<bool, IndexerError> {
        if caller.is_admin {
            return Ok(true);
        }
        let (acl, owner_field) = self.skill_acl(skill).await?.unwrap_or((None, None));
        let policy = self.resolve_policy(acl, Verb::Read).await?;
        let mut effective = self.caller_groups(caller).await?;
        if self.owns(caller, owner_field.as_deref(), fm).await? {
            effective.insert("owner".to_owned());
        }
        Ok(intersects(&policy, &effective))
    }

    /// Whether `caller` may WRITE (create/overwrite) the instance page at
    /// `page_id` with `content`. Symmetric to [`Self::may_read_instance`]
    /// but WITHOUT the public shortcut: a write is allowed only for admin,
    /// or for the resolved owner of BOTH the existing page (no hijack) and
    /// the incoming content (no create-for-/transfer-to another subject).
    /// Public / no-`owner_field` instances are therefore admin-write-only.
    ///
    /// Only `type: instance` pages are gated here (P1); skill/other pages
    /// return `Ok(true)` and keep the existing meta-skill protection.
    pub async fn may_write_page(
        &self,
        caller: &AclCaller<'_>,
        page_id: &str,
        content: &str,
    ) -> Result<bool, IndexerError> {
        if caller.is_admin {
            return Ok(true);
        }
        let parsed = parse(content)?;
        if parsed.frontmatter.page_type != PageType::Instance {
            return Ok(true); // P1: gate instance writes only
        }
        let skill = parsed
            .frontmatter
            .fields
            .get("skill")
            .and_then(YamlValue::as_str)
            .unwrap_or("")
            .to_owned();
        let incoming_fm: Value = serde_json::from_str(&crate::indexer::mapping_to_json(
            &parsed.frontmatter.fields,
        )?)?;
        let existing_fm: Option<Value> = self
            .expand(page_id, None, None)
            .await?
            .map(|e| e.frontmatter);
        self.may_write_instance(caller, &skill, existing_fm.as_ref(), &incoming_fm)
            .await
    }

    /// The deterministic write decision given the caller, the target
    /// `skill`, the existing instance frontmatter (`None` for a create),
    /// and the incoming frontmatter. Admin always passes; otherwise the
    /// caller must be the resolved owner of the existing page (if any) AND
    /// of the incoming content; an unresolved owner fails closed.
    ///
    /// One refinement on the overwrite path: an owner-scoped instance
    /// (`owner_field.is_some()`) whose existing owner is now unresolvable
    /// (`None` — an orphaned/erased tombstone) may be RECLAIMED by a caller
    /// who owns the incoming content, so the rightful owner can re-create
    /// their own erased instance. Public / no-`owner_field` instances are
    /// never reclaimable and stay admin-write-only.
    pub async fn may_write_instance(
        &self,
        caller: &AclCaller<'_>,
        skill: &str,
        existing_fm: Option<&Value>,
        incoming_fm: &Value,
    ) -> Result<bool, IndexerError> {
        if caller.is_admin {
            return Ok(true);
        }
        let (acl, owner_field) = self.skill_acl(skill).await?.unwrap_or((None, None));
        let mut effective = self.caller_groups(caller).await?;

        // The `owner` group resolves differently per write verb (CR §4.4):
        // on CREATE the caller must own the INCOMING content (no
        // create-for-another); on OVERWRITE the caller must own the
        // EXISTING page (no hijack), with one refinement — an owner-scoped
        // instance whose existing owner is now unresolvable is ORPHANED and
        // may be RECLAIMED by a caller who owns the incoming content (so a
        // member who erased their data can re-create it). Public /
        // no-`owner_field` skills never resolve an owner and stay
        // admin-write-only via the policy intersection.
        let verb = match existing_fm {
            Some(existing) => {
                let existing_owner = self
                    .resolve_owner_subject(owner_field.as_deref(), existing)
                    .await?;
                if existing_owner.as_deref() == Some(caller.subject) {
                    effective.insert("owner".to_owned());
                } else if owner_field.is_some()
                    && existing_owner.is_none()
                    && self
                        .resolve_owner_subject(owner_field.as_deref(), incoming_fm)
                        .await?
                        .as_deref()
                        == Some(caller.subject)
                {
                    effective.insert("owner".to_owned()); // orphan reclaim
                }
                Verb::Update
            }
            None => {
                if self
                    .resolve_owner_subject(owner_field.as_deref(), incoming_fm)
                    .await?
                    .as_deref()
                    == Some(caller.subject)
                {
                    effective.insert("owner".to_owned());
                }
                Verb::Create
            }
        };

        let policy = self.resolve_policy(acl, verb).await?;
        Ok(intersects(&policy, &effective))
    }

    /// Whether `caller` may read/append a chat group's history. The chat is
    /// owned by the [`CHAT_OWNER_SKILL`] instance whose id == `chat_group_id`
    /// (its `owner_field` value, e.g. `community_member.credential`). Admin
    /// bypasses. A `chat_group_id` with no owning instance — or an owner that
    /// cannot be resolved — is treated as ungated (compat: non-member chat
    /// groups keep their prior open behaviour), so only chats that DO map to
    /// an owned member become private.
    pub async fn may_access_chat(
        &self,
        caller: &AclCaller<'_>,
        chat_group_id: &str,
    ) -> Result<bool, IndexerError> {
        if caller.is_admin {
            return Ok(true);
        }
        let page_id = format!("markdown/instances/{CHAT_OWNER_SKILL}/{chat_group_id}.md");
        let Some(expanded) = self.expand(&page_id, None, None).await? else {
            return Ok(true); // no owning instance → ungated (compat)
        };
        let owner_field = self
            .skill_acl(CHAT_OWNER_SKILL)
            .await?
            .and_then(|(_, owner_field)| owner_field);
        match self
            .resolve_owner_subject(owner_field.as_deref(), &expanded.frontmatter)
            .await?
        {
            Some(owner) => Ok(owner == caller.subject),
            None => Ok(true), // owner unresolvable → ungated (compat)
        }
    }

    /// The `(acl, owner_field)` a skill declares, or `None` when no such
    /// skill page exists in the tenant index. `acl` is `None` when the
    /// skill declares neither an `acl:` block nor a legacy `visibility:`
    /// field (→ the decision falls through to the tenant default).
    async fn skill_acl(
        &self,
        skill: &str,
    ) -> Result<Option<(Option<AclPolicy>, Option<String>)>, IndexerError> {
        Ok(self
            .list_skills()
            .await?
            .into_iter()
            .find(|s| s.id == skill)
            .map(|s| (s.acl, s.owner_field)))
    }

    /// The caller's effective non-structural group set: `public` (always,
    /// for an authenticated caller) plus the token groups, minus any
    /// reserved name. The structural `owner` group is added by the calling
    /// verb method (it resolves differently for read vs create vs
    /// overwrite); `admin` is handled by the up-front bypass. DuckDB
    /// membership joins in here in a later PR.
    async fn caller_groups(&self, caller: &AclCaller<'_>) -> Result<HashSet<String>, IndexerError> {
        let mut groups = HashSet::new();
        groups.insert("public".to_owned());
        for g in caller.token_groups {
            if !RESERVED_GROUPS.contains(&g.as_str()) {
                groups.insert(g.clone());
            }
        }
        // DuckDB-canonical membership. Reserved names are stripped here too
        // so a stray `group_members` row can never grant a structural group
        // (CR §8.2). The lookup takes the conn lock itself; `caller_groups`
        // is never called while the lock is held, so there is no re-entrancy.
        for g in self.duckdb_groups(caller.subject).await? {
            if !RESERVED_GROUPS.contains(&g.as_str()) {
                groups.insert(g);
            }
        }
        Ok(groups)
    }

    /// Whether `caller` is the resolved owner of the instance whose
    /// frontmatter is `fm`, under the skill's `owner_field`.
    async fn owns(
        &self,
        caller: &AclCaller<'_>,
        owner_field: Option<&str>,
        fm: &Value,
    ) -> Result<bool, IndexerError> {
        Ok(self
            .resolve_owner_subject(owner_field, fm)
            .await?
            .as_deref()
            == Some(caller.subject))
    }

    /// The group list that authorises `verb` for a skill, per the §4.5
    /// resolution order: the skill's declared verb (incl. legacy mapping
    /// folded in by [`crate::read::SkillInfo`]) → tenant default → an
    /// empty list (fail-closed; admin still bypasses upstream).
    async fn resolve_policy(
        &self,
        acl: Option<AclPolicy>,
        verb: Verb,
    ) -> Result<Vec<String>, IndexerError> {
        if let Some(groups) = acl.as_ref().and_then(|p| policy_verb(p, verb)) {
            return Ok(groups.clone());
        }
        let defaults = self.tenant_acl_defaults().await?;
        Ok(policy_verb(&defaults, verb).cloned().unwrap_or_default())
    }

    /// The tenant-wide default policy — the `acl_defaults:` block on the
    /// `escurel` meta-skill page when present, else the shipped default
    /// (open read, admin-only writes) which reproduces today's behaviour.
    /// Read fresh on each decision (no caching) so a meta-skill edit takes
    /// effect immediately; a single indexed row lookup.
    async fn tenant_acl_defaults(&self) -> Result<AclPolicy, IndexerError> {
        let conn = self.conn.lock().await;
        let fm_json: Option<String> = conn
            .query_row(
                "SELECT frontmatter::VARCHAR FROM pages WHERE page_id = ? LIMIT 1",
                duckdb::params![META_SKILL_PAGE_ID],
                |row| row.get::<_, String>(0),
            )
            .ok();
        drop(conn);
        if let Some(json) = fm_json {
            let fm: Value = serde_json::from_str(&json)?;
            if let Some(policy) = crate::read::parse_named_acl(&fm, "acl_defaults") {
                return Ok(policy);
            }
        }
        Ok(shipped_tenant_default())
    }

    /// Resolve an instance's owning principal (`sub`) from its frontmatter
    /// and the skill's `owner_field`. A direct field value is the owner; a
    /// `[[skill::id]]` wikilink is resolved to the linked instance's
    /// `credential`. `None` when the field is absent or unresolvable.
    async fn resolve_owner_subject(
        &self,
        owner_field: Option<&str>,
        fm: &Value,
    ) -> Result<Option<String>, IndexerError> {
        let Some(field) = owner_field else {
            return Ok(None);
        };
        let Some(raw) = fm.get(field).and_then(Value::as_str) else {
            return Ok(None);
        };

        // Wikilink indirection: the owner is the linked instance's
        // `credential` (e.g. event_profile.member → community_member).
        if let Some(wl) = crate::read::first_wikilink_target(raw) {
            let resolved = self.resolve(&wl, None).await?;
            let Some(page) = resolved.page else {
                return Ok(None);
            };
            let owner = self.expand(&page.page_id, None, None).await?.and_then(|e| {
                e.frontmatter
                    .get(OWNER_CREDENTIAL_FIELD)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            return Ok(owner);
        }

        // Direct value (e.g. community_member.credential) is the owner sub.
        Ok(Some(raw.to_owned()))
    }
}

/// Pure allow-list union check: does the caller's effective group set
/// intersect the verb's authorised group list? Empty intersection →
/// deny (fail-closed). No deny rules in v1.
fn intersects(policy: &[String], effective: &HashSet<String>) -> bool {
    policy.iter().any(|g| effective.contains(g))
}
