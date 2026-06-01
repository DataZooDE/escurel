//! Dry-run authoring validation.
//!
//! [`Indexer::validate`] runs the same frontmatter + wikilink
//! checks the live write path ([`Indexer::update_page`]) performs
//! *before* committing — but writes nothing to DuckDB or the
//! LaneStore. It is the engine behind the `validate` agent tool
//! (`docs/contract/agent-interface.md §5`): the authoring-feedback
//! channel that lets an agent see what the indexer would say about
//! a draft without paying for the commit.
//!
//! The v1 check set, kept honest (only checks actually implemented
//! here appear in the output):
//!
//! - **frontmatter parses** as a valid YAML mapping with a
//!   `type:` of `skill` / `instance`. A parse failure is a single
//!   `error`-severity issue with code `frontmatter_parse`.
//! - **required_frontmatter keys present.** When the draft's
//!   `skill:` resolves to a skill page in the index that declares
//!   `required_frontmatter`, every declared key must appear in the
//!   draft's frontmatter; each missing key is an `error` issue with
//!   code `frontmatter_required_key_missing`, located at
//!   `frontmatter.<key>`.
//! - **wikilink syntax parses.** A typed wikilink whose `id`
//!   segment is empty (e.g. `[[customer::]]`) is a `warning` issue
//!   with code `wikilink_parse`.
//! - **referenced skills exist.** Every typed outbound wikilink
//!   `[[<skill>::...]]` whose `<skill>` is not an indexed skill
//!   page is an `error` issue with code `unknown_skill`.

use std::collections::{HashMap, HashSet};

use escurel_md::wikilink::parse_wikilinks;
use escurel_md::{PageType, YamlValue, parse};

use crate::{Indexer, IndexerError};

/// Severity of a validation [`Issue`]. An `error` rejects a live
/// write; a `warning` commits but is surfaced in the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    /// Wire string per `docs/spec/protocol.md §Issue`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One validation finding. Shape mirrors `docs/spec/protocol.md
/// §Issue` (`severity` / `code` / `location` / `message` /
/// optional `suggestion`); the `validate`, `update_page`, and
/// `apply_op` tools all share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    /// Stable machine code, e.g. `unknown_skill`,
    /// `frontmatter_required_key_missing`.
    pub code: String,
    /// Where in the draft, e.g. `frontmatter.name` or `frontmatter`.
    pub location: String,
    pub message: String,
    pub suggestion: Option<String>,
}

impl Issue {
    fn error(code: &str, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: code.to_owned(),
            location: location.into(),
            message: message.into(),
            suggestion: None,
        }
    }

    fn warning(code: &str, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code: code.to_owned(),
            location: location.into(),
            message: message.into(),
            suggestion: None,
        }
    }
}

impl Indexer {
    /// Dry-run the indexer's authoring checks on `content` and
    /// return the resulting [`Issue`] list. Writes nothing.
    ///
    /// `_page_id` is the optional `as_page_id` from the agent tool;
    /// today the checks don't depend on the target page id (the
    /// draft's own `skill:` frontmatter drives the required-key and
    /// skill-existence checks), but the parameter is accepted so the
    /// surface matches the contract and future per-page rules
    /// (e.g. immutability of event instances) have a home.
    ///
    /// # Errors
    ///
    /// Returns [`IndexerError`] only for an underlying DuckDB
    /// failure while looking up skill pages. A malformed draft is
    /// *not* an error — it is reported as an `Issue` so the agent
    /// gets structured feedback rather than an opaque failure.
    pub async fn validate(
        &self,
        _page_id: Option<&str>,
        content: &str,
    ) -> Result<Vec<Issue>, IndexerError> {
        let parsed = match parse(content) {
            Ok(p) => p,
            Err(e) => {
                // A parse failure short-circuits: there is no
                // frontmatter / body to run the remaining checks
                // against. One structured error rather than a panic.
                return Ok(vec![Issue::error(
                    "frontmatter_parse",
                    "frontmatter",
                    e.to_string(),
                )]);
            }
        };

        let mut issues = Vec::new();
        let fields = &parsed.frontmatter.fields;

        // Skill pages declare themselves via `id:`; instance pages
        // via `skill:`.
        let declared_skill = match parsed.frontmatter.page_type {
            PageType::Instance => fields.get("skill").and_then(YamlValue::as_str),
            PageType::Skill => fields.get("id").and_then(YamlValue::as_str),
        };

        // Collect every skill slug we need to resolve up front — the
        // draft's declared skill plus each typed wikilink target — so
        // existence + required_frontmatter resolve in ONE locked pass
        // instead of 2N queries / 2N lock acquisitions across the
        // loops below.
        let wikilinks = parse_wikilinks(parsed.body);
        let mut wanted: HashSet<&str> = HashSet::new();
        if let Some(skill) = declared_skill {
            wanted.insert(skill);
        }
        for wl in &wikilinks {
            if let (Some(skill), Some(_)) = (&wl.skill, &wl.id) {
                wanted.insert(skill.as_str());
            }
        }
        // `skills[slug]` present  => skill exists, value is its
        // required_frontmatter list; absent => not an indexed skill.
        let skills = self.resolve_skills(&wanted).await?;

        // required_frontmatter — only when the draft's declared
        // skill resolves to a skill page that declares required keys.
        if let Some(skill) = declared_skill {
            match skills.get(skill) {
                // A `skill:` on an instance that names a non-existent
                // skill is itself an unknown-skill error.
                None if parsed.frontmatter.page_type == PageType::Instance => {
                    issues.push(Issue::error(
                        "unknown_skill",
                        "frontmatter.skill",
                        format!("declared skill `{skill}` is not an indexed skill page"),
                    ));
                }
                Some(required) => {
                    for key in required {
                        if fields.get(key.as_str()).is_none() {
                            issues.push(Issue::error(
                                "frontmatter_required_key_missing",
                                format!("frontmatter.{key}"),
                                format!("skill `{skill}` requires frontmatter key `{key}`"),
                            ));
                        }
                    }
                }
                None => {}
            }
        }

        // Wikilink syntax + referenced-skill existence.
        for wl in &wikilinks {
            match (&wl.skill, &wl.id) {
                (Some(skill), Some(_)) => {
                    if !skills.contains_key(skill.as_str()) {
                        issues.push(Issue::error(
                            "unknown_skill",
                            format!("wikilink `[[{skill}::...]]`"),
                            format!("wikilink references unknown skill `{skill}`"),
                        ));
                    }
                }
                (Some(skill), None) => {
                    issues.push(Issue::warning(
                        "wikilink_parse",
                        format!("wikilink `[[{skill}::]]`"),
                        format!("typed wikilink `[[{skill}::]]` has an empty id segment"),
                    ));
                }
                // Bare `[[id]]` (no skill) — resolution is deferred
                // to lookup time; nothing to assert here for v1.
                (None, _) => {}
            }
        }

        Ok(issues)
    }

    /// Resolve a set of skill slugs in a single locked DuckDB pass.
    ///
    /// Returns a map keyed by the slugs that exist as indexed skill
    /// pages (`page_type = 'skill'`); each value is that skill's
    /// declared `required_frontmatter` list (empty when it declares
    /// none). A slug absent from the map is not an indexed skill —
    /// callers treat that as an `unknown_skill` issue.
    async fn resolve_skills(
        &self,
        slugs: &HashSet<&str>,
    ) -> Result<HashMap<String, Vec<String>>, IndexerError> {
        let mut out = HashMap::new();
        if slugs.is_empty() {
            return Ok(out);
        }

        // Dynamic `IN (?, ?, …)` with bound params — never string
        // interpolation of the slugs (injection-safe).
        let placeholders = std::iter::repeat_n("?", slugs.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT slug, frontmatter::VARCHAR FROM pages \
             WHERE page_type = 'skill' AND slug IN ({placeholders})"
        );
        let bindings: Vec<String> = slugs.iter().map(|s| (*s).to_owned()).collect();

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn duckdb::ToSql> =
            bindings.iter().map(|b| b as &dyn duckdb::ToSql).collect();
        let mut rows = stmt.query(param_refs.as_slice())?;
        while let Some(row) = rows.next()? {
            let slug: String = row.get(0)?;
            let fm_json: Option<String> = row.get(1)?;
            let required = match fm_json {
                Some(s) => {
                    let fm: serde_json::Value = serde_json::from_str(&s)?;
                    fm.get("required_frontmatter")
                        .and_then(serde_json::Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                }
                None => Vec::new(),
            };
            out.insert(slug, required);
        }
        Ok(out)
    }
}
