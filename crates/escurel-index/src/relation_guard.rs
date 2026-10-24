//! In-use guard for declared relations.
//!
//! A skill declares a relation as a field of `kind: link` with `to: <skill>`.
//! Archiving a page, or renaming its id, would leave every live instance that
//! holds such a link pointing at nothing. [`Indexer::relation_referrers`]
//! lists those instances so the write path can refuse with the generic code
//! [`RELATION_IN_USE`].
//!
//! Scope:
//!
//! - Only links held in a field the referrer's skill declares with `to:` count.
//!   An undeclared wikilink in a body or in an undeclared field is not guarded.
//! - The target's skill may opt out with `relation_guard: false` in its
//!   frontmatter. The default is on.
//! - Batch awareness: `pending` maps page ids to the content a changeset will
//!   write. A pending page replaces its saved version, so a referrer the same
//!   changeset archives or re-points does not block, and a referrer the
//!   changeset creates does.

use std::collections::HashMap;

use escurel_md::wikilink::parse_wikilinks;

use crate::{FieldKind, Indexer, IndexerError, parse_fields};

/// The generic refusal code.
pub const RELATION_IN_USE: &str = "relation_in_use";

/// How many referrers a refusal lists.
pub const RELATION_REFERRER_LIMIT: usize = 20;

/// One live instance that holds a declared link to the guarded page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Referrer {
    pub page_id: String,
    pub field: String,
}

/// Referrers of a page: the first [`RELATION_REFERRER_LIMIT`] and the total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationReferrers {
    pub total: usize,
    pub items: Vec<Referrer>,
}

/// `markdown/instances/<skill>/<id>.md` to `(skill, id)`.
fn split_instance(page_id: &str) -> Option<(&str, &str)> {
    let rest = page_id.strip_prefix("markdown/instances/")?;
    let (skill, file) = rest.split_once('/')?;
    let id = file.strip_suffix(".md")?;
    (!skill.is_empty() && !id.is_empty() && !id.contains('/')).then_some((skill, id))
}

fn links_in(value: &serde_json::Value) -> Vec<(String, String)> {
    let texts: Vec<&str> = match value {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    };
    texts
        .into_iter()
        .flat_map(parse_wikilinks)
        .filter_map(|wl| Some((wl.skill?, wl.id?)))
        .collect()
}

impl Indexer {
    /// Live instances holding a declared link field that points at
    /// `target_page_id`, empty when the page is not an instance, its skill
    /// opted out (`relation_guard: false`) or nothing declares a relation to
    /// that skill.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError`] only for an underlying DuckDB failure.
    pub async fn relation_referrers(
        &self,
        target_page_id: &str,
        pending: &HashMap<String, String>,
    ) -> Result<RelationReferrers, IndexerError> {
        let Some((t_skill, t_id)) = split_instance(target_page_id) else {
            return Ok(RelationReferrers::default());
        };

        // Skill declarations, overlay over base (the later row wins).
        let mut skills: HashMap<String, serde_json::Value> = HashMap::new();
        {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(
                "SELECT slug, frontmatter::VARCHAR FROM pages \
                 WHERE page_type = 'skill' AND NOT COALESCE(deleted, false) \
                 ORDER BY (page_id LIKE 'markdown/base/%') DESC",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let slug: String = row.get(0)?;
                if let Some(text) = row.get::<_, Option<String>>(1)? {
                    skills.insert(slug, serde_json::from_str(&text)?);
                }
            }
        }
        let guarded = skills
            .get(t_skill)
            .and_then(|fm| fm.get("relation_guard"))
            .and_then(serde_json::Value::as_bool)
            != Some(false);
        if !guarded {
            return Ok(RelationReferrers::default());
        }
        // referring skill -> its link fields that point at the target skill
        let mut declared: HashMap<&str, Vec<String>> = HashMap::new();
        for (slug, fm) in &skills {
            let names: Vec<String> = parse_fields(fm)
                .into_iter()
                .filter(|f| f.kind == FieldKind::Link && f.target_skill.as_deref() == Some(t_skill))
                .map(|f| f.name)
                .collect();
            if !names.is_empty() {
                declared.insert(slug.as_str(), names);
            }
        }
        if declared.is_empty() {
            return Ok(RelationReferrers::default());
        }

        // Saved instances, then pending ones replace or add.
        let mut candidates: HashMap<String, (String, serde_json::Value)> = HashMap::new();
        {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(
                "SELECT page_id, skill, frontmatter::VARCHAR FROM pages \
                 WHERE page_type = 'instance' AND scenario IS NULL \
                   AND NOT COALESCE(deleted, false)",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let page_id: String = row.get(0)?;
                let Some(skill) = row.get::<_, Option<String>>(1)? else {
                    continue;
                };
                if !declared.contains_key(skill.as_str()) {
                    continue;
                }
                if let Some(text) = row.get::<_, Option<String>>(2)? {
                    candidates.insert(page_id, (skill, serde_json::from_str(&text)?));
                }
            }
        }
        for (page_id, content) in pending {
            candidates.remove(page_id);
            let Ok(parsed) = escurel_md::parse(content) else {
                continue;
            };
            if parsed.frontmatter.page_kind != escurel_md::PageKind::Instance {
                continue;
            }
            let fm = serde_json::to_value(&parsed.frontmatter.fields).unwrap_or_default();
            let Some(skill) = fm.get("skill").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if declared.contains_key(skill) {
                candidates.insert(page_id.clone(), (skill.to_owned(), fm.clone()));
            }
        }

        let mut found = Vec::new();
        for (page_id, (skill, fm)) in candidates {
            if page_id == target_page_id
                || fm.get("archived").and_then(serde_json::Value::as_bool) == Some(true)
            {
                continue;
            }
            for field in &declared[skill.as_str()] {
                let holds = fm
                    .get(field)
                    .is_some_and(|v| links_in(v).iter().any(|(s, i)| s == t_skill && i == t_id));
                if holds {
                    found.push(Referrer {
                        page_id: page_id.clone(),
                        field: field.clone(),
                    });
                }
            }
        }
        found.sort_by(|a, b| (&a.page_id, &a.field).cmp(&(&b.page_id, &b.field)));
        let total = found.len();
        found.truncate(RELATION_REFERRER_LIMIT);
        Ok(RelationReferrers {
            total,
            items: found,
        })
    }
}

impl RelationReferrers {
    /// The refusal issue, `None` when nothing refers to the page.
    #[must_use]
    pub fn issue(&self, page_id: &str, action: &str) -> Option<crate::Issue> {
        if self.total == 0 {
            return None;
        }
        let list = self
            .items
            .iter()
            .map(|r| format!("`{}` (field `{}`)", r.page_id, r.field))
            .collect::<Vec<_>>()
            .join(", ");
        let more = self.total.saturating_sub(self.items.len());
        let tail = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        Some(crate::Issue::error(
            RELATION_IN_USE,
            "page_id",
            format!(
                "cannot {action} `{page_id}`: {} live instance(s) link to it through a declared \
                 field: {list}{tail}",
                self.total
            ),
        ))
    }
}
