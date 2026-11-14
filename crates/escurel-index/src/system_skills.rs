//! Skills escurel itself ships into every served tenant.
//!
//! The `escurel` meta-skill was the first ([`crate::meta_skill`]); agent
//! profiles (#823) add `agent`, and runner registrations (#818) add `runtime`. Each is one row
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
    /// SHA-256 (hex) of every earlier markdown escurel shipped for this row. A
    /// tenant whose page is still byte-equal to one of them never edited it, so
    /// it is upgraded to [`Self::markdown`]; any other page is the tenant's own
    /// and is kept. Add the outgoing hash here whenever the markdown changes.
    pub previous_sha256: &'static [&'static str],
}

/// Skill id of agent profiles (#823): the one the agent-profile parser keys on.
pub use escurel_types::agent_profile::AGENT_SKILL as AGENT_SKILL_ID;

/// Every skill escurel ships. Adding one is adding a row.
pub const SYSTEM_SKILLS: &[SystemSkill] = &[
    SystemSkill {
        id: crate::meta_skill::META_SKILL_ID,
        page_id: crate::meta_skill::META_SKILL_PAGE_ID,
        markdown: crate::meta_skill::META_SKILL_MD,
        // Operators append tenant guidance to it; its sections are protected
        // by the meta-skill rules instead.
        admin_only: false,
        previous_sha256: &[],
    },
    SystemSkill {
        id: AGENT_SKILL_ID,
        page_id: "markdown/skills/agent.md",
        markdown: include_str!("agent_skill.md"),
        admin_only: true,
        // v2026.11.13 (before `token_epoch`, #829).
        previous_sha256: &["e7b9e157096c11d4e30f0a28e3e12b414d726457fee4407bd7aec7937f072f98"],
    },
    SystemSkill {
        id: escurel_types::runtime::RUNTIME_SKILL,
        page_id: "markdown/skills/runtime.md",
        markdown: include_str!("runtime_skill.md"),
        admin_only: true,
        previous_sha256: &[],
    },
];

/// Whether instances of `skill` (and its skill page) are admin-write-only.
#[must_use]
pub fn is_admin_only_skill(skill: &str) -> bool {
    SYSTEM_SKILLS.iter().any(|s| s.admin_only && s.id == skill)
}

/// Whether `page_id` is admin-write-only by path: an admin-only system skill's
/// page, or an instance under it (`markdown/instances/<skill>/…`).
#[must_use]
pub fn is_admin_only_path(page_id: &str) -> bool {
    is_admin_only_skill_page(page_id)
        || crate::backend::rows::split_instance_page_id(page_id)
            .is_some_and(|(skill, _)| is_admin_only_skill(skill))
}

/// Whether writing `content` at `page_id` touches an admin-only system skill:
/// by path ([`is_admin_only_path`]), or by what the page claims to be — a skill
/// page declaring an admin-only `id:` or an instance declaring an admin-only
/// `skill:`. Unparseable content is judged by its path alone.
///
/// The one predicate behind both the write ACL and the gateway's
/// `ESCUREL_WRITE_ACL` ladder: `off` / `log` relax the ordinary ACL, never
/// this gate (#823).
#[must_use]
pub fn is_admin_only_write(page_id: &str, content: &str) -> bool {
    is_admin_only_path(page_id)
        || escurel_md::parse(content).is_ok_and(|p| declares_admin_only(&p.frontmatter))
}

/// Whether a page's frontmatter claims an admin-only system skill.
pub(crate) fn declares_admin_only(fm: &escurel_md::Frontmatter) -> bool {
    let field = match fm.page_kind {
        escurel_md::PageKind::Skill => "id",
        escurel_md::PageKind::Instance => "skill",
    };
    fm.fields
        .get(field)
        .and_then(escurel_md::YamlValue::as_str)
        .is_some_and(is_admin_only_skill)
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
    fn only_the_agent_and_runtime_skills_are_admin_only() {
        assert!(is_admin_only_skill("agent"));
        assert!(is_admin_only_skill("runtime"));
        assert!(is_admin_only_path("markdown/instances/runtime/laptop.md"));
        assert!(!is_admin_only_skill("escurel"));
        assert!(!is_admin_only_skill("note"));
        assert!(is_admin_only_skill_page("markdown/skills/agent.md"));
        assert!(!is_admin_only_skill_page("markdown/skills/escurel.md"));
    }

    #[test]
    fn an_admin_only_write_is_named_by_path_or_by_claim() {
        let note = "---\nkind: instance\nskill: note\nid: n\n---\n";
        assert!(!is_admin_only_write("markdown/instances/note/n.md", note));
        assert!(is_admin_only_write("markdown/instances/agent/n.md", note));
        assert!(is_admin_only_write("markdown/skills/agent.md", "garbage"));
        let claims_agent = "---\nkind: instance\nskill: agent\nid: n\n---\n";
        assert!(is_admin_only_write(
            "markdown/instances/note/n.md",
            claims_agent
        ));
        let skill_claims_agent = "---\nkind: skill\nid: agent\n---\n";
        assert!(is_admin_only_write(
            "markdown/skills/other.md",
            skill_claims_agent
        ));
    }
}
