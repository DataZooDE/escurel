//! Batch-aware validation for draft sessions.
//!
//! A draft session holds several proposed pages that land together as one
//! changeset. [`Indexer::validate`] judges one page against the saved
//! corpus, so a page that links to a sibling created in the same batch is
//! reported as dangling although the batch would resolve it. This module adds
//! [`Indexer::validate_batch_member`]: the same checks, with the sibling pages
//! treated as present, plus the relation rules a skill declares.
//!
//! A skill declares a relation as a field of `kind: link` with `to: <skill>`:
//!
//! ```yaml
//! fields:
//!   - {name: parent, kind: link, to: note}
//! ```
//!
//! For a draft the value must be a wikilink `[[<skill>::<id>]]` naming an
//! instance of that skill, either saved or created by the same batch. The
//! codes are generic and name no application object:
//!
//! - `relation_invalid`: the value is not a wikilink to an instance of `to`.
//! - `relation_unresolved`: the target is neither saved nor in the batch.
//! - `relation_cycle`: the page is part of a cycle of relations that all point
//!   at pages the batch creates. None of them can land first.
//!
//! The check is limited to drafts. A live write keeps the existing, softer
//! rule (a dangling link is an error only in a `required` field).

use std::collections::{HashMap, HashSet};

use escurel_md::wikilink::parse_wikilinks;
use escurel_md::{PageKind, YamlMapping, YamlValue, parse};

use crate::validate::Issue;
use crate::{FieldKind, Indexer, IndexerError, SkillField};

fn error(code: &str, location: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue::error(code, location, message)
}

/// `markdown/instances/<skill>/<id>.md`
#[must_use]
pub fn instance_page_id(skill: &str, id: &str) -> String {
    format!("markdown/instances/{skill}/{id}.md")
}

/// One relation a page holds: the declaring field, the linked skill and id,
/// and the skill the declaration demands (`None` if the value is no usable
/// wikilink).
struct Relation {
    field: String,
    target: Option<(String, String)>,
}

fn string_values(value: &YamlValue) -> Vec<&str> {
    if let Some(s) = value.as_str() {
        return vec![s];
    }
    value
        .as_sequence()
        .map(|seq| seq.iter().filter_map(YamlValue::as_str).collect())
        .unwrap_or_default()
}

fn relations(fields: &YamlMapping, declared: &[SkillField]) -> Vec<Relation> {
    let mut out = Vec::new();
    for decl in declared {
        let (FieldKind::Link, false) = (decl.kind, decl.target_skills.is_empty()) else {
            continue;
        };
        let Some(value) = fields.get(decl.name.as_str()) else {
            continue;
        };
        for text in string_values(value) {
            let target = parse_wikilinks(text)
                .into_iter()
                .find_map(|wl| Some((wl.skill?, wl.id?)));
            out.push(Relation {
                field: decl.name.clone(),
                target,
            });
        }
    }
    out
}

/// Every typed link in a frontmatter value, keyed by its top-level field.
fn frontmatter_links(fields: &YamlMapping) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (key, value) in fields {
        let Some(key) = key.as_str() else { continue };
        for text in string_values(value) {
            for wl in parse_wikilinks(text) {
                if let (Some(skill), Some(id)) = (wl.skill, wl.id) {
                    out.push((key.to_owned(), skill, id));
                }
            }
        }
    }
    out
}

impl Indexer {
    /// [`Indexer::validate`] for a page proposed as part of a batch.
    ///
    /// `pending` maps the page id of every other page the batch will write to
    /// its proposed content. Compared with `validate`:
    ///
    /// - a dangling-link finding is dropped when the link resolves to a
    ///   pending page;
    /// - every declared `kind: link, to: <skill>` field is checked
    ///   (`relation_invalid`, `relation_unresolved`);
    /// - a cycle of such relations through pages the batch creates is
    ///   `relation_cycle`.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError`] only for an underlying DuckDB failure.
    pub async fn validate_batch_member(
        &self,
        page_id: Option<&str>,
        content: &str,
        pending: &HashMap<String, String>,
    ) -> Result<Vec<Issue>, IndexerError> {
        let mut issues = self
            .validate_with_pending(page_id, content, pending)
            .await?;
        let Ok(parsed) = parse(content) else {
            return Ok(issues);
        };
        if parsed.frontmatter.page_kind != PageKind::Instance {
            return Ok(issues);
        }
        let fields = &parsed.frontmatter.fields;
        let own_skill = fields.get("skill").and_then(YamlValue::as_str);

        // A link the batch resolves is not dangling.
        let batch_has = |skill: &str, id: &str| {
            let target = instance_page_id(skill, id);
            pending.contains_key(&target) || page_id == Some(target.as_str())
        };
        let resolved_by_batch: HashSet<(String, String)> = frontmatter_links(fields)
            .into_iter()
            .filter(|(_, skill, id)| batch_has(skill, id))
            .map(|(key, skill, id)| (format!("frontmatter.{key}"), format!("[[{skill}::{id}]]")))
            .collect();
        issues.retain(|issue| {
            issue.code != "dangling_wikilink"
                || !resolved_by_batch
                    .iter()
                    .any(|(loc, link)| issue.location == *loc && issue.message.contains(link))
        });

        let Some(own_skill) = own_skill else {
            return Ok(issues);
        };
        let mut declared_by_skill: HashMap<String, Vec<SkillField>> = HashMap::new();
        declared_by_skill.insert(own_skill.to_owned(), self.declared_fields(own_skill).await?);
        // A declared relation is judged by the skill-rule engine
        // (`relation_invalid`, `relation_unresolved`); the generic dangling
        // finding at the same place says the same thing less precisely.
        let relation_locations: HashSet<String> = issues
            .iter()
            .filter(|i| i.code.starts_with("relation_"))
            .map(|i| i.location.clone())
            .collect();
        issues.retain(|issue| {
            issue.code != "dangling_wikilink" || !relation_locations.contains(&issue.location)
        });
        let mut declared_by_skill: HashMap<String, Vec<SkillField>> = HashMap::new();
        declared_by_skill.insert(own_skill.to_owned(), self.declared_fields(own_skill).await?);

        if let Some(page_id) = page_id
            && let Some(field) = self
                .relation_cycle_through(page_id, content, pending, &mut declared_by_skill)
                .await?
        {
            issues.push(error(
                "relation_cycle",
                format!("frontmatter.{field}"),
                "this page and the pages it links to in the batch depend on each other; \
                 none of them could be saved first",
            ));
        }
        Ok(issues)
    }

    /// The first field of `page_id` whose relation leads, through pages the
    /// batch creates, back to `page_id`.
    async fn relation_cycle_through(
        &self,
        page_id: &str,
        content: &str,
        pending: &HashMap<String, String>,
        declared_by_skill: &mut HashMap<String, Vec<SkillField>>,
    ) -> Result<Option<String>, IndexerError> {
        let mut nodes: HashMap<&str, &str> = pending
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        nodes.insert(page_id, content);
        let ids: Vec<String> = nodes.keys().map(|k| (*k).to_owned()).collect();
        let saved = self.live_pages(&ids).await?;

        // Edges only run to pages the batch creates: a saved target does not
        // have to wait for anything.
        let mut edges: HashMap<&str, Vec<(String, &str)>> = HashMap::new();
        for (&node, &text) in &nodes {
            if saved.contains(node) {
                continue;
            }
            let Ok(parsed) = parse(text) else { continue };
            let Some(skill) = parsed
                .frontmatter
                .fields
                .get("skill")
                .and_then(YamlValue::as_str)
            else {
                continue;
            };
            if !declared_by_skill.contains_key(skill) {
                let declared = self.declared_fields(skill).await?;
                declared_by_skill.insert(skill.to_owned(), declared);
            }
            for relation in relations(&parsed.frontmatter.fields, &declared_by_skill[skill]) {
                let Some((s, id)) = relation.target else {
                    continue;
                };
                let target = instance_page_id(&s, &id);
                if let Some((&key, _)) = nodes.get_key_value(target.as_str())
                    && !saved.contains(key)
                {
                    edges.entry(node).or_default().push((relation.field, key));
                }
            }
        }

        for (field, first) in edges.get(page_id).cloned().unwrap_or_default() {
            let mut seen = HashSet::new();
            let mut stack = vec![first];
            while let Some(current) = stack.pop() {
                if current == page_id {
                    return Ok(Some(field));
                }
                if !seen.insert(current) {
                    continue;
                }
                for (_, next) in edges.get(current).into_iter().flatten() {
                    stack.push(next);
                }
            }
        }
        Ok(None)
    }

    /// Page ids that the declared relations of `content` point at (`kind:
    /// link, to: <skill>` fields of its skill). Used to order the pages of a
    /// batch so a target lands before the page that links to it.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError`] only for an underlying DuckDB failure.
    pub async fn declared_relation_targets(
        &self,
        content: &str,
    ) -> Result<Vec<String>, IndexerError> {
        let Ok(parsed) = parse(content) else {
            return Ok(Vec::new());
        };
        let Some(skill) = parsed
            .frontmatter
            .fields
            .get("skill")
            .and_then(YamlValue::as_str)
        else {
            return Ok(Vec::new());
        };
        let declared = self.declared_fields(skill).await?;
        Ok(relations(&parsed.frontmatter.fields, &declared)
            .into_iter()
            .filter_map(|r| r.target)
            .map(|(s, id)| instance_page_id(&s, &id))
            .collect())
    }

    /// The `fields:` a skill declares; empty for an unknown skill.
    async fn declared_fields(&self, skill: &str) -> Result<Vec<SkillField>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT frontmatter::VARCHAR FROM pages \
             WHERE page_type = 'skill' AND slug = ? \
             ORDER BY (page_id LIKE 'markdown/base/%') DESC",
        )?;
        let mut rows = stmt.query(duckdb::params![skill])?;
        let mut declared = Vec::new();
        while let Some(row) = rows.next()? {
            // Overlay shadows base: the later row wins.
            if let Some(text) = row.get::<_, Option<String>>(0)? {
                declared = crate::parse_fields(&serde_json::from_str(&text)?);
            }
        }
        Ok(declared)
    }

    /// Which of `page_ids` exist as live pages.
    async fn live_pages(&self, page_ids: &[String]) -> Result<HashSet<String>, IndexerError> {
        let mut out = HashSet::new();
        if page_ids.is_empty() {
            return Ok(out);
        }
        let placeholders = std::iter::repeat_n("?", page_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT page_id FROM pages WHERE page_id IN ({placeholders}) \
             AND NOT COALESCE(deleted, false)"
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn duckdb::ToSql> =
            page_ids.iter().map(|id| id as &dyn duckdb::ToSql).collect();
        let mut rows = stmt.query(refs.as_slice())?;
        while let Some(row) = rows.next()? {
            out.insert(row.get::<_, String>(0)?);
        }
        Ok(out)
    }
}
