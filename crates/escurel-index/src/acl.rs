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

use escurel_md::{PageType, YamlValue, parse};
use serde_json::Value;

use crate::read::Visibility;
use crate::{Indexer, IndexerError};

/// The verified caller, for an instance ACL decision. `subject` is the
/// token `sub` claim; `is_admin` is the admin-role bypass.
#[derive(Debug, Clone, Copy)]
pub struct AclCaller<'a> {
    pub subject: &'a str,
    pub is_admin: bool,
}

/// The frontmatter field that carries a member's owning principal when an
/// owner is reached through a `[[community_member::id]]` wikilink.
const OWNER_CREDENTIAL_FIELD: &str = "credential";

/// The skill whose instance (id == `chat_group_id`) names the owner of a
/// chat group (ADR-13: `chat_group_id` := `community_member_id`). The
/// chat's owner is that instance's `owner_field` value, resolved the same
/// way as an instance owner.
const CHAT_OWNER_SKILL: &str = "community_member";

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
        let Some((visibility, owner_field)) = self.skill_acl(skill).await? else {
            // Unknown skill (no schema page) → no declared policy → public.
            return Ok(true);
        };
        if visibility == Visibility::Public {
            return Ok(true);
        }
        match self
            .resolve_owner_subject(owner_field.as_deref(), fm)
            .await?
        {
            Some(owner) => Ok(owner == caller.subject),
            None => Ok(false), // fail closed: owner-private but unresolved
        }
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
        // Write uses the skill's `owner_field` only — there is no public
        // write shortcut, so a public / unschema'd / no-owner_field skill
        // resolves to no owner and is admin-write-only (fail closed).
        let owner_field = match self.skill_acl(skill).await? {
            Some((_, owner_field)) => owner_field,
            None => None,
        };
        match existing_fm {
            // OVERWRITE/DELETE: the caller must own the EXISTING page (no
            // hijack). Owning it, they may rewrite it however they like —
            // including releasing or tombstoning it (e.g. `/delete-my-data`
            // repoints the owner wikilink at a deleted placeholder), so the
            // incoming owner is NOT re-checked here.
            Some(existing) => Ok(self
                .resolve_owner_subject(owner_field.as_deref(), existing)
                .await?
                .as_deref()
                == Some(caller.subject)),
            // CREATE: the caller must own the INCOMING content (no
            // create-for-/transfer-to another subject). An unresolved owner
            // (public / no `owner_field`) denies → admin-create only.
            None => Ok(self
                .resolve_owner_subject(owner_field.as_deref(), incoming_fm)
                .await?
                .as_deref()
                == Some(caller.subject)),
        }
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

    /// The `(visibility, owner_field)` a skill declares, or `None` when no
    /// such skill page exists in the tenant index.
    async fn skill_acl(
        &self,
        skill: &str,
    ) -> Result<Option<(Visibility, Option<String>)>, IndexerError> {
        Ok(self
            .list_skills()
            .await?
            .into_iter()
            .find(|s| s.id == skill)
            .map(|s| (s.visibility, s.owner_field)))
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
