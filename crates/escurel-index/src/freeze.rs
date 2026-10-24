//! Freezing and protection of instances, declared by their skill.
//!
//! Two declarations, both opt-in:
//!
//! - `freeze_when: {field: value}` (or `{field: [v1, v2]}`) on a skill: while the *stored* page's
//!   `field` equals a value, the page cannot be updated, archived, renamed or drafted against.
//! - `frozen_by_parent: true` on a `kind: link` field: an instance whose link points at a frozen
//!   page is frozen as well. The link is resolved from the stored (or batch) page of the parent,
//!   one by-id read per level, never a scan of the corpus: the cost of a check is
//!   O(links x [`MAX_PARENT_DEPTH`]).
//! - `protected: [delete, move]` on a skill: `delete_page` / archive and `move_page` of its
//!   instances are refused; updates stay allowed.
//!
//! Frozen means frozen: no role is exempt, and a write cannot lift its own freeze, because the
//! check reads the stored page. A freeze ends by editing the skill page (whoever may write skill
//! pages) or by an operator restoring the page; see `docs/spec/protocol.md`.

use std::collections::HashMap;

use escurel_md::{PageKind, YamlMapping, YamlValue, parse};

use crate::field_rules::scalar_text;
use crate::validate::{Issue, SkillContract};
use crate::{FieldKind, Indexer, IndexerError};

/// How many `frozen_by_parent` links a freeze is followed through. A page more than this many
/// links below a frozen page is not frozen by it. A bound on the walk and on cycles, not a
/// modelling limit anyone should need.
pub const MAX_PARENT_DEPTH: usize = 4;

/// The removal actions a skill may protect.
pub const PROTECTABLE: [&str; 2] = ["delete", "move"];

/// `freeze_when:` as `(field, values)`; `None` when absent or ill-formed.
pub(crate) fn parse_freeze_when(fm: &serde_json::Value) -> Option<(String, Vec<String>)> {
    crate::read::parse_required_when(fm.get("freeze_when")?)
}

/// `protected:` as a list of recognised actions, in declaration order. Anything unrecognised is
/// dropped here and reported by [`protected_problem`].
pub(crate) fn parse_protected(fm: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(serde_json::Value::Array(items)) = fm.get("protected") {
        for s in items.iter().filter_map(serde_json::Value::as_str) {
            if PROTECTABLE.contains(&s) && !out.iter().any(|o| o == s) {
                out.push(s.to_owned());
            }
        }
    }
    out
}

/// Why a `protected:` value is malformed, `None` when it is fine.
pub(crate) fn protected_problem(raw: &serde_json::Value) -> Option<&'static str> {
    let ok = raw.as_array().is_some_and(|a| {
        !a.is_empty()
            && a.iter()
                .all(|v| v.as_str().is_some_and(|s| PROTECTABLE.contains(&s)))
    });
    (!ok).then_some("`protected:` must be a non-empty list drawn from `delete` and `move`")
}

/// `freeze_when:` is a one-entry mapping of a field to a value or a non-empty list of plain values.
pub(crate) const FREEZE_WHEN_PROBLEM: &str = "`freeze_when:` must be a one-entry mapping `{field: value}` or `{field: [value, ...]}` with a non-empty list of plain values";

/// `markdown/instances/<skill>/<id>.md`, from a `[[skill::id]]` target.
fn instance_page_id(skill: &str, id: &str) -> String {
    format!("markdown/instances/{skill}/{id}.md")
}

fn is_archived_fields(fields: &YamlMapping) -> bool {
    fields.get("archived").and_then(YamlValue::as_bool) == Some(true)
}

impl Indexer {
    /// The contracts of every skill a page of `skills` could be governed by.
    async fn contracts_of(
        &self,
        skills: &[&str],
    ) -> Result<HashMap<String, SkillContract>, IndexerError> {
        self.resolve_skills(&skills.iter().copied().collect()).await
    }

    /// Why the page `fields` (of skill `skill`) is frozen, `None` when it is not. `own` also
    /// tests the page's own `freeze_when`; without it only the parents decide (the question for
    /// content about to be written, whose own freeze state is the stored page's business).
    ///
    /// `pending` supplies parent pages a changeset writes alongside, ahead of the saved ones.
    async fn frozen_reason(
        &self,
        skill: &str,
        fields: &YamlMapping,
        own: bool,
        pending: &HashMap<String, String>,
        depth: usize,
    ) -> Result<Option<String>, IndexerError> {
        let contracts = self.contracts_of(&[skill]).await?;
        let Some(contract) = contracts.get(skill) else {
            return Ok(None);
        };
        if own
            && let Some((field, values)) = &contract.freeze_when
            && fields
                .get(field.as_str())
                .and_then(scalar_text)
                .is_some_and(|v| values.contains(&v))
        {
            let current = fields
                .get(field.as_str())
                .and_then(scalar_text)
                .unwrap_or_default();
            return Ok(Some(format!("its `{field}` is `{current}`")));
        }
        if depth >= MAX_PARENT_DEPTH {
            return Ok(None);
        }
        for field in contract
            .fields
            .iter()
            .filter(|f| f.frozen_by_parent && f.kind == FieldKind::Link)
        {
            let Some(value) = fields.get(field.name.as_str()) else {
                continue;
            };
            let value = serde_json::to_value(value).unwrap_or_default();
            for (pskill, pid) in crate::relation_guard::links_in(&value) {
                let parent_page = instance_page_id(&pskill, &pid);
                let content = match pending.get(&parent_page) {
                    Some(c) => Some(c.clone()),
                    None => self.read_page_markdown(&parent_page).await?,
                };
                let Some(parsed) = content.as_deref().and_then(|c| parse(c).ok()) else {
                    continue;
                };
                if parsed.frontmatter.page_kind != PageKind::Instance {
                    continue;
                }
                let parent_skill = parsed
                    .frontmatter
                    .fields
                    .get("skill")
                    .and_then(YamlValue::as_str)
                    .unwrap_or(&pskill)
                    .to_owned();
                if let Some(why) = Box::pin(self.frozen_reason(
                    &parent_skill,
                    &parsed.frontmatter.fields,
                    true,
                    pending,
                    depth + 1,
                ))
                .await?
                {
                    return Ok(Some(format!(
                        "`{}` links to `{parent_page}` through `{}`, and {why}",
                        field.name, field.name
                    )));
                }
            }
        }
        Ok(None)
    }

    /// Why the STORED page `page_id` is frozen (own state, or through its parents).
    async fn stored_frozen_reason(
        &self,
        page_id: &str,
        pending: &HashMap<String, String>,
    ) -> Result<Option<String>, IndexerError> {
        let Some(stored) = self.read_page_markdown(page_id).await? else {
            return Ok(None);
        };
        let Ok(parsed) = parse(&stored) else {
            return Ok(None);
        };
        if parsed.frontmatter.page_kind != PageKind::Instance {
            return Ok(None);
        }
        let skill = parsed
            .frontmatter
            .fields
            .get("skill")
            .and_then(YamlValue::as_str)
            .or_else(|| path_skill(page_id));
        let Some(skill) = skill else {
            return Ok(None);
        };
        self.frozen_reason(skill, &parsed.frontmatter.fields, true, pending, 0)
            .await
    }

    /// The `instance_frozen` issue for a write of `content` to `page_id`, `None` when allowed.
    ///
    /// A write is refused when the page as stored is frozen, or when the new content links to a
    /// frozen parent through a `frozen_by_parent` field (a new child, or a child moved under one).
    pub(crate) async fn frozen_write_issues(
        &self,
        page_id: Option<&str>,
        skill: Option<&str>,
        fields: &YamlMapping,
        pending: &HashMap<String, String>,
    ) -> Result<Vec<Issue>, IndexerError> {
        let mut why = None;
        if let Some(page_id) = page_id {
            why = self
                .stored_frozen_reason(page_id, pending)
                .await?
                .map(|w| format!("`{page_id}` is frozen: {w}"));
        }
        if why.is_none()
            && let Some(skill) = skill
        {
            why = self
                .frozen_reason(skill, fields, false, pending, 0)
                .await?
                .map(|w| format!("this instance would be frozen: {w}"));
        }
        Ok(why
            .map(|message| {
                vec![
                    Issue::error("instance_frozen", "page_id", message).with_suggestion(
                        "a frozen page is changed by an operator restore or by editing the skill's `freeze_when`",
                    ),
                ]
            })
            .unwrap_or_default())
    }

    /// `protected: [delete]`: a write that archives an existing, live page.
    pub(crate) async fn protected_archive_issues(
        &self,
        page_id: Option<&str>,
        fields: &YamlMapping,
        skills: &[&SkillContract],
    ) -> Result<Vec<Issue>, IndexerError> {
        let (Some(page_id), true) = (page_id, is_archived_fields(fields)) else {
            return Ok(Vec::new());
        };
        if !skills
            .iter()
            .any(|c| c.protected.iter().any(|p| p == "delete"))
        {
            return Ok(Vec::new());
        }
        let Some(stored) = self.read_page_markdown(page_id).await? else {
            return Ok(Vec::new());
        };
        if parse(&stored).is_ok_and(|p| is_archived_fields(&p.frontmatter.fields)) {
            return Ok(Vec::new());
        }
        Ok(vec![Issue::error(
            "instance_protected",
            "page_id",
            format!("cannot archive `{page_id}`: its skill protects `delete`"),
        )])
    }

    /// The refusal for removing the stored page `page_id` (`action`: `archive` for `delete_page`,
    /// `move` for `move_page`): `instance_frozen`, then `instance_immutable`, then
    /// `instance_protected`. `None` when the page is absent, not an instance, or none applies.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError`] for an underlying DuckDB failure.
    pub async fn removal_issue(
        &self,
        page_id: &str,
        action: &str,
    ) -> Result<Option<Issue>, IndexerError> {
        if let Some(why) = self.stored_frozen_reason(page_id, &HashMap::new()).await? {
            return Ok(Some(Issue::error(
                "instance_frozen",
                "page_id",
                format!("cannot {action} `{page_id}`: it is frozen: {why}"),
            )));
        }
        if let Some(issue) = self.immutable_removal_issue(page_id, action).await? {
            return Ok(Some(issue));
        }
        let Some(stored) = self.read_page_markdown(page_id).await? else {
            return Ok(None);
        };
        let Some(path_skill) = path_skill(page_id) else {
            return Ok(None);
        };
        let mut wanted = vec![path_skill.to_owned()];
        if let Some(s) = parse(&stored).ok().and_then(|p| {
            p.frontmatter
                .fields
                .get("skill")
                .and_then(YamlValue::as_str)
                .map(str::to_owned)
        }) {
            wanted.push(s);
        }
        let refs: Vec<&str> = wanted.iter().map(String::as_str).collect();
        let contracts = self.contracts_of(&refs).await?;
        let verb = if action == "move" { "move" } else { "delete" };
        if contracts
            .values()
            .any(|c| c.protected.iter().any(|p| p == verb))
        {
            return Ok(Some(Issue::error(
                "instance_protected",
                "page_id",
                format!("cannot {action} `{page_id}`: its skill protects `{verb}`"),
            )));
        }
        Ok(None)
    }
}

fn path_skill(page_id: &str) -> Option<&str> {
    page_id
        .strip_prefix("markdown/instances/")?
        .split_once('/')
        .map(|(skill, _)| skill)
}
