//! Who may WRITE a skill page, and so promote a draft of one (`ESCUREL_SKILL_ADMIN_GROUPS`).
//!
//! A skill page declares a type: its fields, its ACL, its agent, its schedule. The
//! per-instance write ACL ([`crate::Indexer::may_write_page`]) judged instance pages
//! only, so any member who passed the ordinary checks could rewrite a skill with
//! `update_page`, or promote anybody's draft of one. This rule closes that: writing a
//! skill page — directly, or by promoting a draft — needs `escurel:admin` or a group the
//! operator names. Proposing stays open (`create_draft`), so authors can still suggest a
//! change for an administrator to accept.
//!
//! A page is a skill page when its id is under `markdown/skills/`, or when its stored or
//! incoming frontmatter says `kind: skill` (a relabel in either direction is a skill
//! write). The rule is part of the write ACL: `ESCUREL_WRITE_ACL=off` skips it and `log`
//! records a would-be refusal, as for every other write-ACL decision.

use std::collections::BTreeSet;

use escurel_md::PageKind;

use crate::acl::AclCaller;
use crate::{Indexer, IndexerError};

/// The operator's rule for skill-page writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SkillWritePolicy {
    /// Only `escurel:admin` (the default: `ESCUREL_SKILL_ADMIN_GROUPS` unset or empty).
    #[default]
    AdminOnly,
    /// `escurel:admin`, or a person in one of these groups (token groups or
    /// `group_members` rows). A machine caller (a run or agent token) is never granted by
    /// a group: a skill change is a person's decision.
    Groups(BTreeSet<String>),
    /// `*`: the earlier behaviour — whoever passes the rest of the write ACL may write.
    Open,
}

impl SkillWritePolicy {
    /// Parse `ESCUREL_SKILL_ADMIN_GROUPS`: empty → [`Self::AdminOnly`], `*` →
    /// [`Self::Open`], else a comma-separated list of group names.
    ///
    /// # Errors
    /// `*` mixed with group names, or a structural group (`public`, `owner`, `admin`) that
    /// would grant nothing, so a typo cannot silently leave the rule admin-only.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let names: Vec<&str> = raw
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        if names.is_empty() {
            return Ok(Self::AdminOnly);
        }
        if names.contains(&"*") {
            return if names.len() == 1 {
                Ok(Self::Open)
            } else {
                Err("`*` (every writer) cannot be combined with group names".to_owned())
            };
        }
        if let Some(reserved) = names
            .iter()
            .find(|n| crate::acl::RESERVED_GROUPS.contains(n))
        {
            return Err(format!(
                "`{reserved}` is a structural group and grants nothing here; name a real group, or \
                 `*` for every writer"
            ));
        }
        Ok(Self::Groups(names.into_iter().map(str::to_owned).collect()))
    }

    /// Who may write, for an error message.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::AdminOnly | Self::Open => "`escurel:admin`".to_owned(),
            Self::Groups(groups) => {
                let named = groups
                    .iter()
                    .map(|g| format!("`{g}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("`escurel:admin` or a member of {named}")
            }
        }
    }
}

/// Whether `page_id` is a skill page by its id alone.
#[must_use]
pub fn is_skill_page_id(page_id: &str) -> bool {
    page_id.starts_with("markdown/skills/")
}

impl Indexer {
    /// Install the operator's skill-page rule. Interior mutability, as for the credential
    /// resolver: the rule belongs to the gateway, and `IndexerHandle::swap` carries it over.
    pub fn set_skill_write_policy(&self, policy: SkillWritePolicy) {
        *self
            .skill_write_policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
    }

    /// The rule in force.
    #[must_use]
    pub fn skill_write_policy(&self) -> SkillWritePolicy {
        self.skill_write_policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether writing `content` at `page_id` is a skill-page write: the id is under
    /// `markdown/skills/`, or the incoming or the stored page says `kind: skill`.
    ///
    /// # Errors
    /// Reading the stored page fails.
    pub async fn is_skill_page_write(
        &self,
        page_id: &str,
        content: &str,
    ) -> Result<bool, IndexerError> {
        if is_skill_page_id(page_id) || declares_skill(content) {
            return Ok(true);
        }
        Ok(self
            .read_page_markdown(page_id)
            .await?
            .is_some_and(|stored| declares_skill(&stored)))
    }

    /// Whether the skill-page rule refuses `caller` writing `content` at `page_id`.
    /// `false` for every page that is not a skill page, for an admin and under
    /// [`SkillWritePolicy::Open`]. Not the whole write decision: the caller still needs the
    /// rest of [`Indexer::may_write_page`].
    ///
    /// # Errors
    /// Reading the stored page or the caller's groups fails.
    pub async fn skill_page_write_denied(
        &self,
        caller: &AclCaller<'_>,
        page_id: &str,
        content: &str,
    ) -> Result<bool, IndexerError> {
        if caller.is_admin {
            return Ok(false);
        }
        let policy = self.skill_write_policy();
        if policy == SkillWritePolicy::Open || !self.is_skill_page_write(page_id, content).await? {
            return Ok(false);
        }
        let SkillWritePolicy::Groups(allowed) = policy else {
            return Ok(true);
        };
        if caller.is_machine() {
            return Ok(true);
        }
        let groups = self.caller_groups(caller).await?;
        Ok(!allowed.iter().any(|g| groups.contains(g)))
    }
}

fn declares_skill(content: &str) -> bool {
    escurel_md::parse(content).is_ok_and(|page| page.frontmatter.page_kind == PageKind::Skill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_forms_and_refuses_what_would_grant_nothing() {
        assert_eq!(SkillWritePolicy::parse(""), Ok(SkillWritePolicy::AdminOnly));
        assert_eq!(
            SkillWritePolicy::parse(" , "),
            Ok(SkillWritePolicy::AdminOnly)
        );
        assert_eq!(SkillWritePolicy::parse(" * "), Ok(SkillWritePolicy::Open));
        assert_eq!(
            SkillWritePolicy::parse("portfolio:tenant-admin, ops"),
            Ok(SkillWritePolicy::Groups(
                ["ops", "portfolio:tenant-admin"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            ))
        );
        assert!(SkillWritePolicy::parse("*,ops").is_err());
        assert!(SkillWritePolicy::parse("public").is_err());
        assert!(SkillWritePolicy::parse("ops,owner").is_err());
    }

    #[test]
    fn a_skill_page_by_id() {
        assert!(is_skill_page_id("markdown/skills/playbook.md"));
        assert!(!is_skill_page_id("markdown/instances/playbook/a.md"));
    }
}
