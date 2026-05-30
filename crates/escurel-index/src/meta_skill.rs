//! The mandatory `escurel` meta-skill.
//!
//! Locked decision 3 (`docs/contract/agent-interface.md`): every
//! tenant ships one mandatory `escurel` skill page — the agent's
//! in-corpus documentation of the tool surface, discovery policy and
//! navigation model ("Read this first when entering a new tenant").
//!
//! This module holds the canonical markdown and the protection rules.
//! The page is auto-shipped at indexer open
//! ([`crate::Indexer::ensure_meta_skill`]) so every *served* tenant
//! exposes it, and protected from removal: an `update_page` that drops
//! the skill identity or one of the standard sections is rejected
//! (operators may *append* tenant-specific guidance, never remove the
//! standard sections).

use escurel_md::{PageType, parse};

/// Skill id (and slug) of the meta-skill.
pub const META_SKILL_ID: &str = "escurel";

/// Canonical lane key / page id for the meta-skill markdown. Mirrors
/// the `seed_from_dir` convention (`markdown/<relpath>`) so
/// [`crate::Indexer::audit`] stays clean.
pub const META_SKILL_PAGE_ID: &str = "markdown/skills/escurel.md";

/// Canonical meta-skill markdown shipped into every tenant. Kept in a
/// sibling `.md` file so the prose stays readable and reviewable.
pub const META_SKILL_MD: &str = include_str!("meta_skill.md");

/// True iff `page_id` targets the meta-skill page.
#[must_use]
pub fn is_meta_skill_page(page_id: &str) -> bool {
    page_id == META_SKILL_PAGE_ID
}

/// The `##` section headers present in `content` (full trimmed header
/// lines, e.g. `"## Tool surface summary"`). The *established*
/// meta-skill's sections form the protected baseline — operators may
/// append new sections but not remove existing ones.
#[must_use]
pub fn section_headers(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim_end)
        .filter(|line| line.starts_with("## "))
        .map(str::to_owned)
        .collect()
}

/// Why a proposed meta-skill rewrite is rejected, or `None` when it is
/// acceptable. The rewrite must (1) stay a skill page named `escurel`
/// and (2) retain every section in `existing_sections` — the sections
/// the currently-established meta-skill carries (empty on first write,
/// so the initial shape — canonical or a tenant's own — is free).
/// Operators append, never remove (`docs/contract/agent-interface.md`
/// locked decision 3).
#[must_use]
pub fn meta_skill_violation(content: &str, existing_sections: &[String]) -> Option<String> {
    let Ok(parsed) = parse(content) else {
        return Some("the `escurel` meta-skill must remain valid markdown".to_owned());
    };
    if parsed.frontmatter.page_type != PageType::Skill {
        return Some("the `escurel` meta-skill must remain a skill page".to_owned());
    }
    let id = parsed
        .frontmatter
        .fields
        .get("id")
        .and_then(escurel_md::YamlValue::as_str);
    if id != Some(META_SKILL_ID) {
        return Some("the `escurel` meta-skill must keep `id: escurel`".to_owned());
    }
    let kept = section_headers(content);
    let missing: Vec<&str> = existing_sections
        .iter()
        .filter(|section| !kept.iter().any(|k| k == *section))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Some(format!(
            "the `escurel` meta-skill must keep its sections; missing: {}",
            missing.join(", ")
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_markdown_retains_its_own_sections() {
        let sections = section_headers(META_SKILL_MD);
        assert!(sections.iter().any(|s| s == "## Tool surface summary"));
        assert_eq!(meta_skill_violation(META_SKILL_MD, &sections), None);
    }

    #[test]
    fn canonical_markdown_declares_the_escurel_skill() {
        let parsed = parse(META_SKILL_MD).expect("meta-skill parses");
        assert_eq!(parsed.frontmatter.page_type, PageType::Skill);
        assert_eq!(
            parsed
                .frontmatter
                .fields
                .get("id")
                .and_then(escurel_md::YamlValue::as_str),
            Some(META_SKILL_ID)
        );
    }

    #[test]
    fn first_write_establishes_any_sections() {
        // No established baseline → a custom meta-skill (e.g. the
        // crm-demo's) is free to ship its own sections.
        let custom = "---\ntype: skill\nid: escurel\n\
                      description: d\nrequired_frontmatter: []\n\
                      optional_frontmatter: []\n---\n# escurel\n\n## Reading order\n\nx\n";
        assert_eq!(meta_skill_violation(custom, &[]), None);
    }

    #[test]
    fn dropping_an_established_section_is_rejected() {
        let established = section_headers(META_SKILL_MD);
        let mangled = META_SKILL_MD.replace("## Anti-patterns", "## Other");
        let violation = meta_skill_violation(&mangled, &established).expect("must be rejected");
        assert!(violation.contains("Anti-patterns"), "got: {violation}");
    }

    #[test]
    fn changing_the_skill_id_is_rejected() {
        let mangled = META_SKILL_MD.replace("id: escurel", "id: not-escurel");
        assert!(meta_skill_violation(&mangled, &[]).is_some());
    }

    #[test]
    fn appending_a_custom_section_is_accepted() {
        let established = section_headers(META_SKILL_MD);
        let extended = format!("{META_SKILL_MD}\n## Tenant-specific notes\n\nHello.\n");
        assert_eq!(meta_skill_violation(&extended, &established), None);
    }
}
