//! Skills escurel itself ships into every served tenant.
//!
//! The `escurel` meta-skill was the first ([`crate::meta_skill`]); agent
//! profiles (#823) add `agent`, and runtimes will add a third. Each is one row
//! of [`SYSTEM_SKILLS`], installed by [`crate::Indexer::ensure_system_skills`]
//! at indexer open.
//!
//! A row marked [`SystemSkill::admin_only`] is authority, not documentation:
//! its page and its instances are writable by the admin role alone, enforced
//! by the write ACL ([`crate::Indexer::may_write_page`]) independently of the
//! page's own `acl:` block — so neither a tenant's widened `acl_defaults`, nor
//! a rewrite of the skill page, nor an instance-level block can open it up.

/// One auto-shipped skill page.
#[derive(Debug, Clone, Copy)]
pub struct SystemSkill {
    /// The skill id (`id:` and slug).
    pub id: &'static str,
    /// The lane key / page id it is written to.
    pub page_id: &'static str,
    /// The canonical markdown.
    pub markdown: &'static str,
    /// Page and instances writable by the admin role only.
    pub admin_only: bool,
}

/// Skill id of agent profiles (#823).
pub const AGENT_SKILL_ID: &str = "agent";

/// Every skill escurel ships. Adding one is adding a row.
pub const SYSTEM_SKILLS: &[SystemSkill] = &[
    SystemSkill {
        id: crate::meta_skill::META_SKILL_ID,
        page_id: crate::meta_skill::META_SKILL_PAGE_ID,
        markdown: crate::meta_skill::META_SKILL_MD,
        // Operators append tenant guidance to it; its sections are protected
        // by the meta-skill rules instead.
        admin_only: false,
    },
    SystemSkill {
        id: AGENT_SKILL_ID,
        page_id: "markdown/skills/agent.md",
        markdown: include_str!("agent_skill.md"),
        admin_only: true,
    },
];

/// Whether instances of `skill` (and its skill page) are admin-write-only.
#[must_use]
pub fn is_admin_only_skill(skill: &str) -> bool {
    SYSTEM_SKILLS.iter().any(|s| s.admin_only && s.id == skill)
}

/// Whether `page_id` is the skill page of an admin-only system skill.
#[must_use]
pub fn is_admin_only_skill_page(page_id: &str) -> bool {
    SYSTEM_SKILLS
        .iter()
        .any(|s| s.admin_only && s.page_id == page_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use escurel_md::{PageKind, YamlValue, parse};

    #[test]
    fn every_row_parses_as_the_skill_it_names() {
        for row in SYSTEM_SKILLS {
            let parsed = parse(row.markdown).expect("parses");
            assert_eq!(parsed.frontmatter.page_kind, PageKind::Skill, "{}", row.id);
            assert_eq!(
                parsed
                    .frontmatter
                    .fields
                    .get("id")
                    .and_then(YamlValue::as_str),
                Some(row.id)
            );
            assert_eq!(row.page_id, format!("markdown/skills/{}.md", row.id));
        }
    }

    #[test]
    fn only_the_agent_skill_is_admin_only() {
        assert!(is_admin_only_skill("agent"));
        assert!(!is_admin_only_skill("escurel"));
        assert!(!is_admin_only_skill("note"));
        assert!(is_admin_only_skill_page("markdown/skills/agent.md"));
        assert!(!is_admin_only_skill_page("markdown/skills/escurel.md"));
    }
}
