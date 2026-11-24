//! A skill page's `search:` block: whether its instances take part in a
//! default search, and which fields reveal one by exact value.
//!
//! ```yaml
//! search:
//!   hidden: true          # optional, default false
//!   exact: [code, serial] # optional, default []
//! ```
//!
//! `hidden: true` keeps the skill's INSTANCES out of a `search` that names no
//! `skill` and no `page_id`, in every lane and before the top-`k` cut. The
//! skill page itself stays searchable: it is catalogue. A hidden instance is
//! still returned by such a search when a query variant (trimmed, compared
//! case-insensitively) equals its page id, its slug, its frontmatter `id`, or
//! the scalar value of one of the skill's `exact` fields. List and mapping
//! values never match. The lookup is one bounded query over `pages`
//! ([`Indexer::exact_hidden_hits`]); its hits go through the same read ACL as
//! every other lane.
//!
//! The lookup reads stored pages, so it covers page-backed instances (markdown
//! and `sql_view`). The rows of an `instances: rows` skill are not pages: a
//! hidden rows skill is reached only with `skill:` or `list_instances`, and
//! declaring it is the warning `search_hidden_rows`.
//!
//! A malformed block is refused on write with `search_invalid`; an `exact`
//! entry that the skill does not declare is the warning `search_exact_unknown`
//! (only when the skill declares any field).

use std::collections::HashSet;

use escurel_md::{PageKind, YamlMapping, YamlValue};

use crate::search::SearchHit;
use crate::validate::Issue;
use crate::{Indexer, IndexerError, SkillInfo};

/// The keys a `search:` block may carry.
const KEYS: [&str; 2] = ["hidden", "exact"];

/// Upper bound on the rows one exact lookup reads before the frontmatter
/// filter and the ACL (identity matches are read first); the caller keeps at
/// most `k` of what survives.
const EXACT_ROW_LIMIT: usize = 200;

/// The parsed `search:` block of a skill page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchSetting {
    pub hidden: bool,
    pub exact: Vec<String>,
}

/// A frontmatter field name usable in an `exact` list: a letter or `_`, then
/// letters, digits, `_` or `-`. The name is spliced into a JSON path, so this
/// is also what keeps the lookup's SQL closed.
#[must_use]
pub fn is_field_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `search:` as written on a skill page. Lenient: only `hidden: true` hides,
/// and only well-formed field names are kept; [`check_search`] reports the rest.
#[must_use]
pub fn parse_search(fm: &serde_json::Value) -> SearchSetting {
    let Some(block) = fm.get("search").and_then(serde_json::Value::as_object) else {
        return SearchSetting::default();
    };
    let mut exact: Vec<String> = Vec::new();
    if let Some(items) = block.get("exact").and_then(serde_json::Value::as_array) {
        for s in items.iter().filter_map(serde_json::Value::as_str) {
            if is_field_name(s) && !exact.iter().any(|e| e == s) {
                exact.push(s.to_owned());
            }
        }
    }
    SearchSetting {
        hidden: block.get("hidden") == Some(&serde_json::Value::Bool(true)),
        exact,
    }
}

/// Lint a skill page's `search:` block. Errors are `search_invalid`
/// (blocking on write); an `exact` field the skill does not declare is the
/// warning `search_exact_unknown` (only when the skill declares any field).
pub(crate) fn check_search(page_kind: PageKind, fields: &YamlMapping) -> Vec<Issue> {
    if page_kind != PageKind::Skill {
        return Vec::new();
    }
    let Some(raw) = fields.get("search") else {
        return Vec::new();
    };
    let invalid = |location: &str, message: String| {
        Issue::error("search_invalid", format!("frontmatter.{location}"), message)
            .with_suggestion("search: {hidden: true, exact: [code]}")
    };
    let Some(block) = raw.as_mapping() else {
        return vec![invalid(
            "search",
            "`search:` must be a mapping `{hidden: <bool>, exact: [<field>, ...]}`".to_owned(),
        )];
    };
    let mut issues = Vec::new();
    for key in block.keys() {
        if !key.as_str().is_some_and(|k| KEYS.contains(&k)) {
            issues.push(invalid(
                "search",
                format!(
                    "`search:` has an unknown key {}; it takes `hidden` and `exact`",
                    describe(key)
                ),
            ));
        }
    }
    let is_rows = fields
        .get("backend")
        .and_then(YamlValue::as_mapping)
        .and_then(|b| b.get("instances"))
        .and_then(YamlValue::as_str)
        == Some("rows");
    if is_rows && block.get("hidden").and_then(YamlValue::as_bool) == Some(true) {
        issues.push(
            Issue::warning(
                "search_hidden_rows",
                "frontmatter.search.hidden",
                "the rows of an `instances: rows` skill are not pages: hidden, they leave default \
                 search entirely and no exact id or `exact` value brings one back; search them \
                 with `skill:` or `list_instances`",
            )
            .with_suggestion("drop `hidden: true`, or find the rows by `skill:`"),
        );
    }
    if let Some(v) = block.get("hidden")
        && v.as_bool().is_none()
    {
        issues.push(invalid(
            "search.hidden",
            "`search.hidden` must be `true` or `false`".to_owned(),
        ));
    }
    let Some(exact) = block.get("exact") else {
        return issues;
    };
    let Some(items) = exact.as_sequence() else {
        issues.push(invalid(
            "search.exact",
            "`search.exact` must be a list of frontmatter field names".to_owned(),
        ));
        return issues;
    };
    let mut names = Vec::new();
    for item in items {
        match item.as_str().filter(|s| is_field_name(s)) {
            Some(s) => names.push(s),
            None => issues.push(invalid(
                "search.exact",
                format!(
                    "`search.exact` entry {} is not a field name (a letter or `_`, then letters, \
                     digits, `_` or `-`)",
                    describe(item)
                ),
            )),
        }
    }
    let declared = declared_fields(fields);
    if !declared.is_empty() {
        for name in names {
            if name != "id" && !declared.contains(name) {
                issues.push(
                    Issue::warning(
                        "search_exact_unknown",
                        "frontmatter.search.exact",
                        format!(
                            "`search.exact` names `{name}`, which the skill does not declare \
                             (`fields:`, `required_frontmatter`, `optional_frontmatter`)"
                        ),
                    )
                    .with_suggestion("declare the field under `fields:`"),
                );
            }
        }
    }
    issues
}

fn describe(v: &YamlValue) -> String {
    match v {
        YamlValue::String(s) => format!("`{s}`"),
        other => serde_json::to_string(other).unwrap_or_else(|_| "?".to_owned()),
    }
}

/// Every field name a skill page declares: `fields:`, `required_frontmatter`
/// and `optional_frontmatter`.
fn declared_fields(fields: &YamlMapping) -> HashSet<String> {
    let fm = serde_json::to_value(fields).unwrap_or_default();
    let mut out: HashSet<String> = crate::parse_fields(&fm)
        .into_iter()
        .map(|f| f.name)
        .collect();
    for key in ["required_frontmatter", "optional_frontmatter"] {
        if let Some(items) = fm.get(key).and_then(serde_json::Value::as_array) {
            out.extend(
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned),
            );
        }
    }
    out
}

/// The hidden skills of a catalogue, as `(id, exact fields)`.
#[must_use]
pub fn hidden_skills(skills: &[SkillInfo]) -> Vec<(&str, &[String])> {
    skills
        .iter()
        .filter(|s| s.search.hidden)
        .map(|s| (s.id.as_str(), s.search.exact.as_slice()))
        .collect()
}

/// `(page_id, slug, skill, frontmatter JSON, first block body)` of one exact-lookup row.
type ExactRow = (String, Option<String>, String, String, Option<String>);

/// The constraints an exact lookup honours, the way the native lane does.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExactScope<'a> {
    /// `Some(Skill)` asks for skill pages only: an exact lookup (instances) has nothing to add.
    pub page_kind: Option<PageKind>,
    pub as_of: Option<&'a str>,
    pub scenario: Option<&'a str>,
    pub filter: Option<&'a serde_json::Value>,
}

/// The lowercase, trimmed, de-duplicated query variants an exact lookup
/// compares against.
fn needles(queries: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    queries
        .iter()
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty() && seen.insert(q.clone()))
        .collect()
}

/// The SQL of one exact lookup and its bind parameters. Field names are
/// checked with [`is_field_name`] (a name that fails is skipped) before they
/// are spliced into a JSON path; every value is bound.
fn exact_sql(
    needles: &[String],
    hidden: &[(&str, &[String])],
    scope: ExactScope<'_>,
) -> (String, Vec<String>) {
    let mut params: Vec<String> = Vec::new();
    let values = vec!["(?)"; needles.len()].join(", ");
    params.extend(needles.iter().cloned());
    let skills = vec!["?"; hidden.len()].join(", ");
    params.extend(hidden.iter().map(|(id, _)| (*id).to_owned()));
    // An identity match (page id, slug, `id`) names one page; it is ordered ahead of field
    // matches, so many rows sharing an `exact` value cannot crowd it past the row bound.
    let identity = "(lower(p.page_id) IN (SELECT v FROM q) \
                    OR lower(p.slug) IN (SELECT v FROM q) \
                    OR lower(json_extract_string(p.frontmatter, '$.id')) IN (SELECT v FROM q))";
    let mut matches = vec![identity.to_owned()];
    for (id, exact) in hidden {
        for field in exact.iter().filter(|f| is_field_name(f)) {
            let path = format!("'$.\"{field}\"'");
            matches.push(format!(
                "(p.skill = ? AND json_type(p.frontmatter, {path}) NOT IN ('ARRAY', 'OBJECT', 'NULL') \
                 AND lower(json_extract_string(p.frontmatter, {path})) IN (SELECT v FROM q))"
            ));
            params.push((*id).to_owned());
        }
    }
    let mut sql = format!(
        "WITH q(v) AS (VALUES {values}) \
         SELECT p.page_id, p.slug, p.skill, p.frontmatter::VARCHAR, \
                (SELECT b.body FROM blocks b WHERE b.page_id = p.page_id \
                 ORDER BY b.ordinal NULLS LAST LIMIT 1) \
         FROM pages p \
         WHERE p.page_type = 'instance' AND p.skill IN ({skills}) AND ({})",
        matches.join(" OR ")
    );
    if let Some(ts) = scope.as_of {
        sql.push_str(" AND (p.at_ts <= ? OR p.at_ts IS NULL)");
        params.push(ts.to_owned());
    }
    match scope.scenario {
        Some(sc) => {
            sql.push_str(" AND (p.scenario = ? OR p.scenario IS NULL)");
            params.push(sc.to_owned());
        }
        None => sql.push_str(" AND p.scenario IS NULL"),
    }
    sql.push_str(&format!(
        " ORDER BY CASE WHEN {identity} THEN 0 ELSE 1 END, p.page_id LIMIT {EXACT_ROW_LIMIT}"
    ));
    (sql, params)
}

impl Indexer {
    /// The instances of `hidden` skills that one of `queries` names exactly
    /// (page id, slug, frontmatter `id`, or an `exact` field's scalar value;
    /// trimmed, case-insensitive), under `scope`: identity matches first, then
    /// by page id, at most [`EXACT_ROW_LIMIT`] rows read. Page-grain hits, not
    /// yet ACL-filtered and not cut to `k`: the caller filters them like any
    /// lane and cuts after, so another owner's row cannot take a caller's slot.
    pub async fn exact_hidden_hits(
        &self,
        queries: &[String],
        hidden: &[(&str, &[String])],
        scope: ExactScope<'_>,
    ) -> Result<Vec<SearchHit>, IndexerError> {
        let needles = needles(queries);
        if hidden.is_empty() || needles.is_empty() || scope.page_kind == Some(PageKind::Skill) {
            return Ok(Vec::new());
        }
        let (sql, params) = exact_sql(&needles, hidden, scope);
        let rows: Vec<ExactRow> = {
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?
        };
        let mut hits = Vec::new();
        for (page_id, slug, skill, fm_json, body) in rows {
            let frontmatter_excerpt: serde_json::Value = serde_json::from_str(&fm_json)?;
            if let Some(f) = scope.filter
                && !crate::filter::matches_filter(f, &frontmatter_excerpt)
            {
                continue;
            }
            hits.push(SearchHit {
                page_id,
                slug,
                skill,
                page_kind: PageKind::Instance,
                anchor: None,
                snippet: crate::search::snippet_from_body(body.as_deref().unwrap_or_default()),
                score: 0.0,
                similarity: 0.0,
                frontmatter_excerpt,
            });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(yaml: &str) -> YamlMapping {
        escurel_md::parse(&format!("---\nkind: skill\nid: s\n{yaml}---\n# s\n"))
            .expect("parse")
            .frontmatter
            .fields
    }

    fn codes(yaml: &str) -> Vec<String> {
        check_search(PageKind::Skill, &fm(yaml))
            .into_iter()
            .map(|i| i.code)
            .collect()
    }

    #[test]
    fn parses_the_block() {
        let v = serde_json::json!({ "search": { "hidden": true, "exact": ["code", "code", "a b", 3] } });
        assert_eq!(
            parse_search(&v),
            SearchSetting {
                hidden: true,
                exact: vec!["code".to_owned()]
            }
        );
        assert_eq!(
            parse_search(&serde_json::json!({})),
            SearchSetting::default()
        );
        // Only a boolean `true` hides.
        let v = serde_json::json!({ "search": { "hidden": "true" } });
        assert!(!parse_search(&v).hidden);
        let v = serde_json::json!({ "search": true });
        assert!(!parse_search(&v).hidden);
    }

    #[test]
    fn field_names() {
        for ok in ["code", "_x", "serial-no", "a1_b"] {
            assert!(is_field_name(ok), "{ok}");
        }
        for bad in ["", "1a", "a b", "a.b", "a\"b", "a'b", "-a", "ä"] {
            assert!(!is_field_name(bad), "{bad}");
        }
    }

    #[test]
    fn malformed_blocks_are_search_invalid() {
        for yaml in [
            "search: true\n",
            "search: [hidden]\n",
            "search: {hidden: 1}\n",
            "search: {exact: code}\n",
            "search: {exact: [1]}\n",
            "search: {exact: [\"a.b\"]}\n",
            "search: {hidden: true, rank: 3}\n",
        ] {
            assert!(codes(yaml).contains(&"search_invalid".to_owned()), "{yaml}");
        }
        assert!(codes("search: {hidden: true, exact: [code]}\n").is_empty());
        assert!(codes("search: {}\n").is_empty());
        // Instance pages are not linted.
        assert!(check_search(PageKind::Instance, &fm("search: true\n")).is_empty());
    }

    #[test]
    fn undeclared_exact_field_is_a_warning_when_fields_are_declared() {
        let yaml = "fields:\n  - {name: code, kind: string}\nrequired_frontmatter: [serial]\n\
                    search: {exact: [code, serial, id, other]}\n";
        assert_eq!(codes(yaml), vec!["search_exact_unknown".to_owned()]);
        // Nothing declared: nothing to compare against.
        assert!(codes("search: {exact: [other]}\n").is_empty());
    }

    #[test]
    fn hiding_a_rows_skill_is_a_warning() {
        let yaml = "backend:\n  kind: sql_view\n  instances: rows\n  key: [id]\n\
                    search: {hidden: true}\n";
        assert_eq!(codes(yaml), vec!["search_hidden_rows".to_owned()]);
        let yaml = "backend:\n  kind: sql_view\n  instances: rows\n  key: [id]\n\
                    search: {hidden: false}\n";
        assert!(codes(yaml).is_empty());
    }

    #[test]
    fn needles_are_trimmed_lowercased_and_unique() {
        let q = vec![" NR-1 ".to_owned(), "nr-1".to_owned(), "  ".to_owned()];
        assert_eq!(needles(&q), vec!["nr-1".to_owned()]);
    }

    #[test]
    fn the_sql_binds_every_value_and_splices_only_field_names() {
        let exact = vec!["code".to_owned(), "x'); DROP TABLE pages; --".to_owned()];
        let hidden = [("note-row", exact.as_slice())];
        let (sql, params) = exact_sql(&["nr-1".to_owned()], &hidden, ExactScope::default());
        assert!(sql.contains("'$.\"code\"'"), "{sql}");
        assert!(!sql.contains("DROP"), "{sql}");
        assert!(sql.contains("p.scenario IS NULL"), "{sql}");
        // Identity matches are read first, so shared field values cannot crowd them out.
        assert!(
            sql.contains("ORDER BY CASE WHEN (lower(p.page_id)"),
            "{sql}"
        );
        // needle, skill IN, skill of the `code` arm.
        assert_eq!(params, vec!["nr-1", "note-row", "note-row"]);
    }
}
