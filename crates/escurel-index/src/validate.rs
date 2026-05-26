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

use duckdb::params;
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

        // required_frontmatter — only when the draft's declared
        // skill resolves to a skill page that declares required keys.
        // Skill pages declare themselves via `id:`; instance pages
        // via `skill:`.
        let skill_id = match parsed.frontmatter.page_type {
            PageType::Instance => fields.get("skill").and_then(YamlValue::as_str),
            PageType::Skill => fields.get("id").and_then(YamlValue::as_str),
        };
        if let Some(skill) = skill_id {
            // A `skill:` on an instance that names a non-existent
            // skill is itself an unknown-skill error.
            if parsed.frontmatter.page_type == PageType::Instance
                && !self.skill_page_exists(skill).await?
            {
                issues.push(Issue::error(
                    "unknown_skill",
                    "frontmatter.skill",
                    format!("declared skill `{skill}` is not an indexed skill page"),
                ));
            } else if let Some(required) = self.required_frontmatter_for(skill).await? {
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
        }

        // Wikilink syntax + referenced-skill existence.
        let wikilinks = parse_wikilinks(parsed.body);
        for wl in &wikilinks {
            match (&wl.skill, &wl.id) {
                (Some(skill), Some(_)) => {
                    if !self.skill_page_exists(skill).await? {
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

    /// True iff a skill page with `frontmatter.id == skill` (i.e.
    /// `pages.slug = skill AND page_type = 'skill'`) is indexed.
    async fn skill_page_exists(&self, skill: &str) -> Result<bool, IndexerError> {
        let conn = self.conn.lock().await;
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM pages WHERE page_type = 'skill' AND slug = ?",
            params![skill],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// The `required_frontmatter` list a skill page declares, or
    /// `None` when the skill is not indexed (the caller treats a
    /// missing skill page as an `unknown_skill` issue separately).
    async fn required_frontmatter_for(
        &self,
        skill: &str,
    ) -> Result<Option<Vec<String>>, IndexerError> {
        let conn = self.conn.lock().await;
        let fm_json: Option<String> = conn
            .query_row(
                "SELECT frontmatter::VARCHAR FROM pages \
                 WHERE page_type = 'skill' AND slug = ? LIMIT 1",
                params![skill],
                |row| row.get(0),
            )
            .ok();
        let Some(fm_json) = fm_json else {
            return Ok(None);
        };
        let fm: serde_json::Value = serde_json::from_str(&fm_json)?;
        let keys = fm
            .get("required_frontmatter")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(Some(keys))
    }
}
