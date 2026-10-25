//! Table-driven validators that a skill declares as data.
//!
//! A skill page states rules in its frontmatter; [`Indexer::validate`] applies
//! them to every instance of that skill, in the saved corpus and in a batch of
//! pending (not yet saved) pages. No rule names a particular skill: the same
//! tables serve any tenant model.
//!
//! Declarations (all opt-in; a skill that declares none behaves as before):
//!
//! | Where | Key | Meaning | Issue code |
//! |-------|-----|---------|------------|
//! | field | `immutable: true` | the key cannot change once the page exists | `field_immutable` |
//! | skill | `immutable: true` | the page cannot change, be archived or be renamed once it exists | `instance_immutable` |
//! | field | `non_blank: true` | a present value is not empty or whitespace | `field_blank` |
//! | field | `format: date` | a string value is `YYYY-MM-DD` | `frontmatter_field_type` |
//! | field | `required_when: {other: value}` or `{other: [v1, v2]}` | required while `other` equals the value, or any of the values | `field_required_when` |
//! | field | `kind: link`, `to: <skill>` | the value is a wikilink to an existing instance of `<skill>` | `relation_invalid`, `relation_unresolved` |
//! | field | `acyclic: true` (with a link to its own skill) | following the link never returns to the start | `relation_cycle` |
//! | skill | `unique: [[a, b]]` (or `{keys: [[a, b]], scope: accepted}`) | the composite key identifies at most one instance | `identity_duplicate` |
//! | skill | `relation_guard: false` | opt out of the in-use guard on archive and rename | `skill_rule_malformed` if not a boolean |
//! | skill | `strict_required_frontmatter: true` | required keys must also be non-blank | `frontmatter_required_key_missing_strict`, `frontmatter_required_key_blank_strict` |
//! | skill | `pack: {id, version, sha256}` | an approved release | `pack_malformed` |
//! | instance | `pack_id`, `pack_version`, `pack_sha256` | the page claims a release; the hash must match the declared one | `pack_unknown`, `pack_hash_mismatch` |
//!
//! Enum values, number bounds and kinds were already enforced by
//! `check_field_value` (`frontmatter_enum_value`, `frontmatter_field_range`,
//! `frontmatter_field_type`); those codes are unchanged.

use std::collections::{HashMap, HashSet};

use escurel_md::wikilink::parse_wikilinks;
use escurel_md::{PageKind, YamlMapping, YamlValue, parse};

use crate::validate::{Issue, SkillContract};
use crate::{Indexer, IndexerError, SkillField};

/// Upper bound on a link chain walk, a guard against a corrupt corpus rather
/// than a limit anyone should reach.
const MAX_CHAIN: usize = 10_000;

/// The scalar a frontmatter value stands for, as text: a string as itself, a
/// number or boolean through its YAML spelling. `None` for null, lists and maps.
pub(crate) fn scalar_text(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::String(s) => Some(s.clone()),
        YamlValue::Bool(b) => Some(b.to_string()),
        YamlValue::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Empty, whitespace-only or null.
pub(crate) fn is_blank(value: &YamlValue) -> bool {
    value.is_null() || value.as_str().is_some_and(|t| t.trim().is_empty())
}

/// `[[skill::id]]` as one whole value.
fn link_target(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    if !(t.starts_with("[[") && t.ends_with("]]")) {
        return None;
    }
    let mut links = parse_wikilinks(t);
    if links.len() != 1 {
        return None;
    }
    let wl = links.remove(0);
    match (wl.skill, wl.id) {
        (Some(s), Some(i)) if !s.is_empty() && !i.is_empty() => Some((s, i)),
        _ => None,
    }
}

/// An instance page in the pending batch.
struct PendingInstance {
    page_id: String,
    skill: String,
    id: String,
    archived: bool,
    fields: YamlMapping,
}

fn pending_instances(pending: &HashMap<String, String>) -> Vec<PendingInstance> {
    let mut out = Vec::new();
    for (page_id, content) in pending {
        let Ok(parsed) = parse(content) else { continue };
        if parsed.frontmatter.page_kind != PageKind::Instance {
            continue;
        }
        let f = parsed.frontmatter.fields;
        let (Some(skill), Some(id)) = (
            f.get("skill").and_then(YamlValue::as_str),
            f.get("id").and_then(YamlValue::as_str),
        ) else {
            continue;
        };
        out.push(PendingInstance {
            page_id: page_id.clone(),
            skill: skill.to_owned(),
            id: id.to_owned(),
            archived: f.get("archived").and_then(YamlValue::as_bool) == Some(true),
            fields: f.clone(),
        });
    }
    out
}

/// What a link target looks like to the walkers: its fields, from the pending
/// batch first, then from the index.
enum Target {
    Missing,
    Found(serde_json::Value),
}

/// Lint the rule declarations on a SKILL page. Reported once, on the page
/// that declares them, rather than on every instance.
pub(crate) fn check_skill_rules(page_kind: PageKind, fields: &YamlMapping) -> Vec<Issue> {
    if page_kind != PageKind::Skill {
        return Vec::new();
    }
    let mut issues = Vec::new();
    let bad = |key: &str, message: &str| {
        Issue::error(
            "skill_rule_malformed",
            format!("frontmatter.{key}"),
            message,
        )
    };
    for key in ["immutable", "strict_required_frontmatter", "relation_guard"] {
        if let Some(v) = fields.get(key)
            && v.as_bool().is_none()
        {
            issues.push(bad(key, &format!("`{key}:` must be `true` or `false`")));
        }
    }
    let declared: HashSet<String> = {
        let fm = serde_json::to_value(fields).unwrap_or_default();
        crate::parse_fields(&fm)
            .into_iter()
            .map(|f| f.name)
            .collect()
    };
    if fields.contains_key("unique") {
        let fm = serde_json::to_value(fields).unwrap_or_default();
        match parse_unique(&fm) {
            Some((keys, _)) if !keys.is_empty() && keys.iter().all(|k| !k.is_empty()) => {
                for key in keys.iter().flatten() {
                    if !declared.is_empty() && !declared.contains(key) {
                        issues.push(
                            Issue::warning(
                                "unique_field_unknown",
                                "frontmatter.unique",
                                format!(
                                    "`unique:` names `{key}`, which `fields:` does not declare"
                                ),
                            )
                            .with_suggestion("declare the key under `fields:` so it is typed too"),
                        );
                    }
                }
            }
            _ => {
                issues.push(
                    bad(
                        "unique",
                        "`unique:` must be a list of key lists, e.g. `[[system, external_id]]`, \
                         or `{keys: [[...]], scope: accepted | all}`",
                    )
                    .with_suggestion("unique: [[system, external_id]]"),
                );
            }
        }
    }
    if let Some(raw) = fields.get("pack") {
        let entries: Vec<&YamlValue> = match raw {
            YamlValue::Sequence(items) => items.iter().collect(),
            other => vec![other],
        };
        for (i, e) in entries.iter().enumerate() {
            let text = |k: &str| {
                e.as_mapping()
                    .and_then(|m| m.get(k))
                    .and_then(YamlValue::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            };
            let hash_ok = text("sha256").is_some_and(|h| {
                h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
            if text("id").is_none() || text("version").is_none() || !hash_ok {
                issues.push(
                    bad(
                        &format!("pack[{i}]"),
                        "a `pack:` release needs a non-empty `id`, `version` and a 64-digit \
                         lowercase hex `sha256`",
                    )
                    .with_suggestion(
                        "pack: {id: starter, version: \"2026-01-01\", sha256: <64 hex>}",
                    ),
                );
            }
        }
    }
    issues
}

/// Lint the rule attributes on the `fields:` entries of a SKILL page.
pub(crate) fn check_field_rules(page_kind: PageKind, fields: &YamlMapping) -> Vec<Issue> {
    if page_kind != PageKind::Skill {
        return Vec::new();
    }
    let Some(raw) = fields.get("fields") else {
        return Vec::new();
    };
    let fm = serde_json::to_value(fields).unwrap_or_default();
    let parsed = crate::parse_fields(&fm);
    let names: HashSet<&str> = parsed.iter().map(|f| f.name.as_str()).collect();
    let raw_json = serde_json::to_value(raw).unwrap_or_default();
    let attrs_of = |name: &str| -> Option<serde_json::Value> {
        match &raw_json {
            serde_json::Value::Array(a) => a
                .iter()
                .find(|i| i.get("name").and_then(|n| n.as_str()) == Some(name))
                .cloned(),
            serde_json::Value::Object(o) => o.get(name).cloned(),
            _ => None,
        }
    };
    let mut issues = Vec::new();
    for f in &parsed {
        let loc = format!("frontmatter.fields.{}", f.name);
        let attrs = attrs_of(&f.name).unwrap_or_default();
        for key in ["immutable", "non_blank", "acyclic"] {
            if let Some(v) = attrs.get(key)
                && !v.is_boolean()
            {
                issues.push(Issue::error(
                    "fields_malformed",
                    format!("{loc}.{key}"),
                    format!("`{key}:` on field `{}` must be `true` or `false`", f.name),
                ));
            }
        }
        if f.acyclic && (f.kind != crate::FieldKind::Link || f.target_skill.is_none()) {
            issues.push(Issue::error(
                "fields_malformed",
                format!("{loc}.acyclic"),
                format!(
                    "`acyclic: true` on `{}` needs `kind: link` and a `to:` skill; with no target \
                     there is no chain to follow",
                    f.name
                ),
            ));
        }
        if let Some(v) = attrs.get("required_when")
            && crate::read::parse_required_when(v).is_none()
        {
            issues.push(
                Issue::error(
                    "fields_malformed",
                    format!("{loc}.required_when"),
                    format!(
                        "`required_when:` on `{}` must be a one-entry mapping `{{other_field: value}}` or `{{other_field: [value, ...]}}` with a non-empty list of plain values",
                        f.name
                    ),
                )
                .with_suggestion("required_when: {status: [resolved, dismissed]}"),
            );
        }
        if let Some((other, _)) = &f.required_when
            && !names.contains(other.as_str())
        {
            issues.push(Issue::warning(
                "field_required_when_unknown",
                format!("{loc}.required_when"),
                format!(
                    "`required_when:` on `{}` reads `{other}`, which `fields:` does not declare",
                    f.name
                ),
            ));
        }
        if let Some(fmt) = &f.format
            && fmt != "date"
        {
            issues.push(Issue::warning(
                "field_format_unknown",
                format!("{loc}.format"),
                format!(
                    "`format: {fmt}` on `{}` is not recognised and constrains nothing",
                    f.name
                ),
            ));
        }
    }
    issues
}

/// `unique:` as a list of key tuples plus whether it counts only accepted
/// instances. `None` when the shape is not one of the two documented forms.
pub(crate) fn parse_unique(fm: &serde_json::Value) -> Option<(Vec<Vec<String>>, bool)> {
    let raw = fm.get("unique")?;
    let (list, scope) = match raw {
        serde_json::Value::Array(_) => (raw, None),
        serde_json::Value::Object(o) => (
            o.get("keys")?,
            o.get("scope").and_then(serde_json::Value::as_str),
        ),
        _ => return None,
    };
    let accepted = match scope {
        None | Some("all") => false,
        Some("accepted") => true,
        Some(_) => return None,
    };
    let mut tuples = Vec::new();
    for item in list.as_array()? {
        let keys = match item {
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(a) => a
                .iter()
                .map(|k| k.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        tuples.push(keys);
    }
    Some((tuples, accepted))
}

impl Indexer {
    /// The saved frontmatter of the live base instance `[[skill::id]]`.
    async fn live_instance_fields(
        &self,
        skill: &str,
        id: &str,
    ) -> Result<Option<serde_json::Value>, IndexerError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT frontmatter::VARCHAR FROM pages \
             WHERE page_type = 'instance' AND skill = ? AND slug = ? \
               AND scenario IS NULL AND NOT COALESCE(deleted, false) LIMIT 1",
        )?;
        let mut rows = stmt.query(duckdb::params![skill, id])?;
        match rows.next()? {
            Some(row) => {
                let text: Option<String> = row.get(0)?;
                Ok(Some(text.map_or(Ok(serde_json::json!({})), |t| {
                    serde_json::from_str(&t)
                })?))
            }
            None => Ok(None),
        }
    }

    async fn resolve_target(
        &self,
        skill: &str,
        id: &str,
        pending: &[PendingInstance],
    ) -> Result<Target, IndexerError> {
        if let Some(p) = pending.iter().find(|p| p.skill == skill && p.id == id) {
            if p.archived {
                return Ok(Target::Missing);
            }
            return Ok(Target::Found(
                serde_json::to_value(&p.fields).unwrap_or_default(),
            ));
        }
        Ok(match self.live_instance_fields(skill, id).await? {
            Some(v) if v.get("archived").and_then(serde_json::Value::as_bool) != Some(true) => {
                Target::Found(v)
            }
            _ => Target::Missing,
        })
    }

    /// Relation integrity for the `kind: link` fields that name a target skill.
    async fn relation_issues(
        &self,
        skill: &str,
        contract: &SkillContract,
        fields: &YamlMapping,
        pending: &[PendingInstance],
    ) -> Result<Vec<Issue>, IndexerError> {
        let mut issues = Vec::new();
        let self_id = fields.get("id").and_then(YamlValue::as_str);
        for field in &contract.fields {
            let (crate::FieldKind::Link, Some(to)) = (field.kind, field.target_skill.as_deref())
            else {
                continue;
            };
            let Some(value) = fields.get(field.name.as_str()) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            let loc = format!("frontmatter.{}", field.name);
            let items: Vec<&YamlValue> = match value {
                YamlValue::Sequence(items) => items.iter().collect(),
                other => vec![other],
            };
            let single = !value.is_sequence();
            for item in items {
                let Some(text) = item.as_str() else {
                    issues.push(Issue::error(
                        "relation_invalid",
                        loc.clone(),
                        format!("`{}` must hold quoted `[[{to}::<id>]]` links", field.name),
                    ));
                    continue;
                };
                if text.trim().is_empty() {
                    // A blank link is `non_blank`'s and `required`'s business.
                    continue;
                }
                let Some((t_skill, t_id)) = link_target(text) else {
                    issues.push(Issue::error(
                        "relation_invalid",
                        loc.clone(),
                        format!("`{}` must be one quoted `[[{to}::<id>]]` link", field.name),
                    ));
                    continue;
                };
                if t_skill != to {
                    issues.push(Issue::error(
                        "relation_invalid",
                        loc.clone(),
                        format!(
                            "`{}` must link to an instance of `{to}`, not `{t_skill}`",
                            field.name
                        ),
                    ));
                    continue;
                }
                let target = self.resolve_target(&t_skill, &t_id, pending).await?;
                if matches!(target, Target::Missing) {
                    issues.push(Issue::error(
                        "relation_unresolved",
                        loc.clone(),
                        format!(
                            "`{}` links to `[[{to}::{t_id}]]`, which is not a live instance \
                             in the corpus or in this batch",
                            field.name
                        ),
                    ));
                    continue;
                }
                if field.acyclic
                    && single
                    && to == skill
                    && let Some(start) = self_id
                    && let Some(at) = self.cycle_at(field, skill, start, &t_id, pending).await?
                {
                    issues.push(Issue::error(
                        "relation_cycle",
                        loc.clone(),
                        format!(
                            "following `{}` from `{start}` reaches `{at}` twice",
                            field.name
                        ),
                    ));
                }
            }
        }
        Ok(issues)
    }

    /// Walk `field` from `first` along instances of `skill`; the id reached
    /// twice, if the chain loops (including back to `start`).
    async fn cycle_at(
        &self,
        field: &SkillField,
        skill: &str,
        start: &str,
        first: &str,
        pending: &[PendingInstance],
    ) -> Result<Option<String>, IndexerError> {
        let mut seen: HashSet<String> = HashSet::from([start.to_owned()]);
        let mut current = first.to_owned();
        for _ in 0..MAX_CHAIN {
            if !seen.insert(current.clone()) {
                return Ok(Some(current));
            }
            let Target::Found(fm) = self.resolve_target(skill, &current, pending).await? else {
                return Ok(None);
            };
            let next = fm
                .get(&field.name)
                .and_then(serde_json::Value::as_str)
                .and_then(link_target);
            match next {
                Some((s, id)) if s == skill => current = id,
                _ => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    /// Composite-identity uniqueness among the saved instances and the pending
    /// batch, excluding the page being validated.
    async fn identity_issues(
        &self,
        page_id: Option<&str>,
        skill: &str,
        contract: &SkillContract,
        fields: &YamlMapping,
        pending: &[PendingInstance],
    ) -> Result<Vec<Issue>, IndexerError> {
        let mut issues = Vec::new();
        let own_page = page_id.map(str::to_owned).or_else(|| {
            fields
                .get("id")
                .and_then(YamlValue::as_str)
                .map(|id| format!("markdown/instances/{skill}/{id}.md"))
        });
        let is_accepted = |status: Option<&str>| status == Some("accepted");
        if contract.unique_accepted_only
            && !is_accepted(fields.get("status").and_then(YamlValue::as_str))
        {
            return Ok(issues);
        }
        for keys in &contract.unique {
            let Some(first) = keys.first() else { continue };
            // An incomplete identity cannot collide; `required` reports the gap.
            let mine: Option<Vec<String>> = keys
                .iter()
                .map(|k| {
                    fields
                        .get(k.as_str())
                        .and_then(scalar_text)
                        .filter(|t| !t.trim().is_empty())
                })
                .collect();
            let Some(mine) = mine else { continue };
            let mut peer: Option<String> = None;
            let live = self
                .list_instances(skill, None, None, Some((first, &mine[0])), None, None)
                .await?;
            for p in live {
                if own_page.as_deref() == Some(p.page_id.as_str()) {
                    continue;
                }
                if contract.unique_accepted_only
                    && !is_accepted(
                        p.frontmatter
                            .get("status")
                            .and_then(serde_json::Value::as_str),
                    )
                {
                    continue;
                }
                let same = keys.iter().zip(&mine).all(|(k, v)| {
                    p.frontmatter.get(k).is_some_and(|sv| match sv {
                        serde_json::Value::String(s) => s == v,
                        serde_json::Value::Null => false,
                        other => &other.to_string() == v,
                    })
                });
                if same {
                    peer = Some(p.page_id);
                    break;
                }
            }
            if peer.is_none() {
                for p in pending.iter().filter(|p| p.skill == skill && !p.archived) {
                    if own_page.as_deref() == Some(p.page_id.as_str()) {
                        continue;
                    }
                    if contract.unique_accepted_only
                        && !is_accepted(p.fields.get("status").and_then(YamlValue::as_str))
                    {
                        continue;
                    }
                    let same = keys.iter().zip(&mine).all(|(k, v)| {
                        p.fields.get(k.as_str()).and_then(scalar_text).as_deref() == Some(v)
                    });
                    if same {
                        peer = Some(p.page_id.clone());
                        break;
                    }
                }
            }
            if let Some(peer) = peer {
                issues.push(Issue::error(
                    "identity_duplicate",
                    format!("frontmatter.{first}"),
                    format!(
                        "`{skill}` identity ({}) already belongs to `{peer}`",
                        keys.join(", ")
                    ),
                ));
            }
        }
        Ok(issues)
    }

    /// Every rule above that needs the corpus, for one instance whose skill
    /// resolved to `contract`.
    pub(crate) async fn declared_rule_issues(
        &self,
        page_id: Option<&str>,
        skill: &str,
        contract: &SkillContract,
        fields: &YamlMapping,
        pending: &HashMap<String, String>,
    ) -> Result<Vec<Issue>, IndexerError> {
        let pending = pending_instances(pending);
        let mut issues = Vec::new();

        // required_when: a conditional `required:`.
        for field in &contract.fields {
            let Some((other, expected)) = &field.required_when else {
                continue;
            };
            let active = fields
                .get(other.as_str())
                .and_then(scalar_text)
                .is_some_and(|v| expected.contains(&v));
            if active && fields.get(field.name.as_str()).is_none_or(is_blank) {
                issues.push(Issue::error(
                    "field_required_when",
                    format!("frontmatter.{}", field.name),
                    format!(
                        "`{}` is required while `{other}` is {}",
                        field.name,
                        expected
                            .iter()
                            .map(|v| format!("`{v}`"))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    ),
                ));
            }
        }

        issues.extend(
            self.relation_issues(skill, contract, fields, &pending)
                .await?,
        );
        issues.extend(
            self.identity_issues(page_id, skill, contract, fields, &pending)
                .await?,
        );
        Ok(issues)
    }

    /// Immutability: the page, or a field of it, differs from what is stored.
    pub(crate) async fn immutability_issues(
        &self,
        page_id: Option<&str>,
        content: &str,
        skills: &[&SkillContract],
        fields: &YamlMapping,
    ) -> Result<Vec<Issue>, IndexerError> {
        let Some(page_id) = page_id else {
            return Ok(Vec::new());
        };
        let Some(stored) = self.read_page_markdown(page_id).await? else {
            return Ok(Vec::new());
        };
        if stored == content {
            return Ok(Vec::new());
        }
        if skills.iter().any(|c| c.immutable) {
            return Ok(vec![Issue::error(
                "instance_immutable",
                "page_id",
                format!(
                    "`{page_id}` is immutable once created; write a new instance to correct it"
                ),
            )]);
        }
        let Ok(old) = parse(&stored) else {
            return Ok(Vec::new());
        };
        let mut issues = Vec::new();
        let mut seen = HashSet::new();
        for field in skills
            .iter()
            .flat_map(|c| &c.fields)
            .filter(|f| f.immutable)
        {
            if !seen.insert(field.name.as_str()) {
                continue;
            }
            let before = old.frontmatter.fields.get(field.name.as_str());
            let after = fields.get(field.name.as_str());
            if before != after {
                issues.push(Issue::error(
                    "field_immutable",
                    format!("frontmatter.{}", field.name),
                    format!("`{}` cannot change once the instance exists", field.name),
                ));
            }
        }
        Ok(issues)
    }

    /// `immutable: true` on a skill also forbids removing its instances.
    ///
    /// `action` is `archive` (`delete_page`) or `move` (`move_page`); the
    /// refusal carries the same `instance_immutable` code as a changed write.
    /// The skill is read from the page path and from the stored page's own
    /// `skill:` key, as for a write, so relabelling does not shed the rule.
    /// `None` when the page is absent, not an instance, or of a mutable skill.
    /// `purge_page` is deliberately not covered: it is the operator's erasure
    /// act, and it only acts on a page that is already archived.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError`] for an underlying DuckDB failure.
    pub async fn immutable_removal_issue(
        &self,
        page_id: &str,
        action: &str,
    ) -> Result<Option<Issue>, IndexerError> {
        let Some(path_skill) = page_id
            .strip_prefix("markdown/instances/")
            .and_then(|tail| tail.split_once('/'))
            .map(|(skill, _)| skill)
        else {
            return Ok(None);
        };
        let Some(stored) = self.read_page_markdown(page_id).await? else {
            return Ok(None);
        };
        let stored_skill = parse(&stored).ok().and_then(|p| {
            p.frontmatter
                .fields
                .get("skill")
                .and_then(YamlValue::as_str)
                .map(str::to_owned)
        });
        let mut wanted: HashSet<&str> = HashSet::from([path_skill]);
        if let Some(s) = stored_skill.as_deref() {
            wanted.insert(s);
        }
        let contracts = self.resolve_skills(&wanted).await?;
        if !contracts.values().any(|c| c.immutable) {
            return Ok(None);
        }
        Ok(Some(Issue::error(
            "instance_immutable",
            "page_id",
            format!(
                "cannot {action} `{page_id}`: its skill is immutable, so an instance is never \
                 changed, archived or renamed; write a new instance to correct it"
            ),
        )))
    }

    /// A page that claims a release (`pack_id`, `pack_version`, `pack_sha256`)
    /// must match the hash a skill page declared for it.
    pub(crate) async fn pack_binding_issues(
        &self,
        fields: &YamlMapping,
        pending: &HashMap<String, String>,
    ) -> Result<Vec<Issue>, IndexerError> {
        let get = |k: &str| fields.get(k).and_then(YamlValue::as_str).map(str::trim);
        let (id, version, sha) = (get("pack_id"), get("pack_version"), get("pack_sha256"));
        if id.is_none() && version.is_none() && sha.is_none() {
            return Ok(Vec::new());
        }
        let (Some(id), Some(version), Some(sha)) = (id, version, sha) else {
            return Ok(vec![
                Issue::error(
                    "pack_malformed",
                    "frontmatter.pack_id",
                    "a page that names a release needs `pack_id`, `pack_version` and `pack_sha256` together",
                )
                .with_suggestion("pack_id, pack_version and pack_sha256"),
            ]);
        };
        let releases = self.declared_releases(pending).await?;
        match releases.get(&format!("{id}@{version}")) {
            None => Ok(vec![Issue::error(
                "pack_unknown",
                "frontmatter.pack_id",
                format!("no skill declares a release `{id}@{version}`"),
            )]),
            Some(expected) if expected != &sha.to_ascii_lowercase() => Ok(vec![Issue::error(
                "pack_hash_mismatch",
                "frontmatter.pack_sha256",
                format!(
                    "`{id}@{version}` is declared with a different sha256 than this page carries"
                ),
            )]),
            Some(_) => Ok(Vec::new()),
        }
    }

    /// `id@version` to sha256, from every skill page's `pack:` declaration,
    /// the saved ones first so a pending declaration of the same release wins.
    async fn declared_releases(
        &self,
        pending: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, IndexerError> {
        let mut manifests: Vec<serde_json::Value> = Vec::new();
        {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(
                "SELECT frontmatter::VARCHAR FROM pages \
                 WHERE page_type = 'skill' AND json_extract(frontmatter, '$.pack') IS NOT NULL \
                 ORDER BY (page_id LIKE 'markdown/base/%') DESC",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let text: Option<String> = row.get(0)?;
                if let Some(t) = text
                    && let Ok(v) = serde_json::from_str(&t)
                {
                    manifests.push(v);
                }
            }
        }
        for content in pending.values() {
            if let Ok(p) = parse(content)
                && p.frontmatter.page_kind == PageKind::Skill
                && let Ok(v) = serde_json::to_value(&p.frontmatter.fields)
            {
                manifests.push(v);
            }
        }
        let mut out = HashMap::new();
        for fm in manifests {
            let entries: Vec<&serde_json::Value> = match fm.get("pack") {
                Some(serde_json::Value::Array(a)) => a.iter().collect(),
                Some(v) => vec![v],
                None => continue,
            };
            for e in entries {
                let text = |k: &str| e.get(k).and_then(serde_json::Value::as_str);
                if let (Some(id), Some(ver), Some(sha)) =
                    (text("id"), text("version"), text("sha256"))
                {
                    out.insert(format!("{id}@{ver}"), sha.to_ascii_lowercase());
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unique_accepts_the_list_form_and_the_scoped_form() {
        assert_eq!(
            parse_unique(&json!({"unique": [["a", "b"], "c"]})),
            Some((vec![vec!["a".into(), "b".into()], vec!["c".into()]], false))
        );
        assert_eq!(
            parse_unique(&json!({"unique": {"keys": [["a"]], "scope": "accepted"}})),
            Some((vec![vec!["a".into()]], true))
        );
        assert_eq!(
            parse_unique(&json!({"unique": {"keys": [["a"]], "scope": "all"}})),
            Some((vec![vec!["a".into()]], false))
        );
    }

    #[test]
    fn unique_rejects_other_shapes() {
        assert_eq!(parse_unique(&json!({"unique": "a"})), None);
        assert_eq!(parse_unique(&json!({"unique": [[1]]})), None);
        assert_eq!(
            parse_unique(&json!({"unique": {"keys": [["a"]], "scope": "x"}})),
            None
        );
        assert_eq!(parse_unique(&json!({})), None);
    }

    #[test]
    fn link_target_takes_exactly_one_whole_link() {
        assert_eq!(
            link_target(" [[topic::root]] "),
            Some(("topic".into(), "root".into()))
        );
        assert_eq!(link_target("see [[topic::root]]"), None);
        assert_eq!(link_target("[[topic::a]] [[topic::b]]"), None);
        assert_eq!(link_target("[[root]]"), None);
        assert_eq!(link_target("topic::root"), None);
    }

    #[test]
    fn scalar_text_spells_numbers_and_booleans() {
        assert_eq!(scalar_text(&YamlValue::from(12)), Some("12".into()));
        assert_eq!(scalar_text(&YamlValue::Bool(true)), Some("true".into()));
        assert_eq!(scalar_text(&YamlValue::Null), None);
    }
}
