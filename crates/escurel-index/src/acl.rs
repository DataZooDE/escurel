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
