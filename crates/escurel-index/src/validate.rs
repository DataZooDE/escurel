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
//! - **`params:` is declarable.** On a SKILL page, a `params:` block
//!   that is neither a sequence nor a mapping — or an entry with no
//!   `name:` — is an `error` issue with code
//!   `frontmatter_params_malformed`; a `kind:` outside the renderable
//!   set is a `warning` with code `frontmatter_param_kind_unknown`.

use std::collections::{HashMap, HashSet};

use escurel_md::wikilink::{WikilinkParsed, parse_wikilinks};
use escurel_md::{PageType, YamlMapping, YamlValue, parse};

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

    /// Attach the `suggestion` field. Worth doing where the fix is a closed
    /// set the author can be handed verbatim.
    #[must_use]
    fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

/// The `autonomy:` check (heron#5 / CR-1), on SKILL pages only.
///
/// Scoped to skill pages because that is where the key is declared: the
/// policy belongs to the skill, and every write derived from it inherits it.
/// On an instance page `autonomy:` remains ordinary free-form frontmatter;
/// narrowing it there would be a behaviour change for pages that predate the
/// key rather than a check.
///
/// Silence on ABSENCE is load-bearing: a skill that declares no policy is not
/// making a mistake, it is declining to declare — and the consumer's own
/// fail-closed default (review) already covers that. The finding fires only
/// when an author reached for the key and missed, which is the one case
/// nothing else in the system can see.
fn check_autonomy(page_type: PageType, fields: &YamlMapping) -> Option<Issue> {
    if page_type != PageType::Skill {
        return None;
    }
    let raw = fields.get("autonomy")?;
    let recognised: Vec<&str> = crate::Autonomy::recognised()
        .iter()
        .map(|a| a.as_str())
        .collect();
    let suggestion = format!("use one of: {}", recognised.join(" | "));

    // A non-string value (`autonomy: [auto]`, `autonomy: true`, or a bare
    // `autonomy:` with nothing after it) is as much a mis-declaration as a
    // misspelt one, and lands on the same finding rather than being ignored.
    let Some(value) = raw.as_str() else {
        return Some(
            Issue::error(
                "frontmatter_autonomy_unknown",
                "frontmatter.autonomy",
                "`autonomy:` must be a string naming the human-in-the-loop policy",
            )
            .with_suggestion(suggestion),
        );
    };
    if crate::Autonomy::parse(value).is_some() {
        return None;
    }
    Some(
        Issue::error(
            "frontmatter_autonomy_unknown",
            "frontmatter.autonomy",
            format!(
                "`autonomy: {value}` is not a recognised human-in-the-loop policy; \
                 a consumer treats it as undeclared and holds writes for review"
            ),
        )
        .with_suggestion(suggestion),
    )
}

/// The `params:` checks (heron#11 / CR-7), on SKILL pages only.
///
/// The scoping is not cosmetic: `params:` is ALREADY taken on instance
/// pages. A `[[query::*]]` page declares `params:` with a `type:` drawn from
/// a different, richer vocabulary (`date`, `number`) and binds the values as
/// SQL parameters. Running these checks there would start emitting findings
/// against a surface that has shipped for releases.
///
/// Two findings, at deliberately different severities:
///
/// - `frontmatter_params_malformed` (**error**) — the block is neither a
///   sequence nor a mapping, or a sequence entry has no `name`. Nothing can
///   be rendered from it: a parameter with no name has nothing to be passed
///   under, so there is no degraded form to fall back to.
/// - `frontmatter_param_kind_unknown` (**warning**) — a `kind:` outside the
///   renderable set. The catalogue reports the parameter as `string` and the
///   form still works, so failing the write would be a behaviour change for
///   a key that has never been validated. Compare `autonomy:`, which is
///   error-severity because there the failure mode is an ungated write.
fn check_params(page_type: PageType, fields: &YamlMapping) -> Vec<Issue> {
    if page_type != PageType::Skill {
        return Vec::new();
    }
    let Some(raw) = fields.get("params") else {
        return Vec::new();
    };
    let recognised: Vec<&str> = crate::ParamKind::recognised()
        .iter()
        .map(|k| k.as_str())
        .collect();
    let suggestion = format!("use one of: {}", recognised.join(" | "));
    let malformed = |message: &str| {
        vec![
            Issue::error(
                "frontmatter_params_malformed",
                "frontmatter.params",
                message,
            )
            .with_suggestion("e.g. `- {name: window, kind: string, required: true}`"),
        ]
    };

    // (declared name, declared kind) per entry.
    let entries: Vec<(String, Option<&YamlValue>)> = if let Some(seq) = raw.as_sequence() {
        let mut out = Vec::new();
        for item in seq {
            let m = item.as_mapping();
            let Some(name) = m.and_then(|m| m.get("name")).and_then(YamlValue::as_str) else {
                return malformed(
                    "every `params:` entry must be a mapping with a `name:` — \
                     a parameter with no name cannot be passed to a run",
                );
            };
            out.push((
                name.to_owned(),
                m.and_then(|m| m.get("kind").or_else(|| m.get("type"))),
            ));
        }
        out
    } else if let Some(map) = raw.as_mapping() {
        map.iter()
            .filter_map(|(k, v)| {
                let name = k.as_str()?;
                let attrs = v.as_mapping();
                Some((
                    name.to_owned(),
                    attrs.and_then(|m| m.get("kind").or_else(|| m.get("type"))),
                ))
            })
            .collect()
    } else {
        return malformed(
            "`params:` must be a sequence of `{name, kind, required}` entries \
             or a mapping of name to those attributes",
        );
    };

    entries
        .into_iter()
        .filter_map(|(name, kind)| {
            // No `kind:` at all is not a finding: an undeclared kind is a
            // text field, which is what an author who omitted it meant.
            let declared = kind?;
            if declared
                .as_str()
                .is_some_and(|s| crate::ParamKind::parse(s).is_some())
            {
                return None;
            }
            let shown = declared
                .as_str()
                .map_or_else(|| format!("{declared:?}"), str::to_owned);
            Some(
                Issue::warning(
                    "frontmatter_param_kind_unknown",
                    format!("frontmatter.params.{name}.kind"),
                    format!(
                        "`kind: {shown}` on param `{name}` is not a renderable kind; \
                         it is reported as `string`, so a client renders a text field"
                    ),
                )
                .with_suggestion(suggestion.clone()),
            )
        })
        .collect()
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

        // The human-in-the-loop policy a skill declares (heron#5 / CR-1).
        // Cheap, local, and independent of every skill lookup below.
        issues.extend(check_autonomy(parsed.frontmatter.page_type, fields));
        // The invocation-parameter block a skill declares (heron#11 / CR-7).
        issues.extend(check_params(parsed.frontmatter.page_type, fields));

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
        // Body links AND frontmatter links. Only the body was parsed
        // before, so `about: "[[nosuchskill::x]]"` sailed through while the
        // identical link one line lower was rejected — and `about:`,
        // `customer:` and `continues:` are where the load-bearing links
        // actually live.
        let body_links = parse_wikilinks(parsed.body);
        let fm_links = Self::frontmatter_wikilinks(fields);

        let mut wanted: HashSet<&str> = HashSet::new();
        if let Some(skill) = declared_skill {
            wanted.insert(skill);
        }
        for wl in body_links.iter().chain(fm_links.iter().map(|(_, wl)| wl)) {
            if let (Some(skill), Some(_)) = (&wl.skill, &wl.id) {
                wanted.insert(skill.as_str());
            }
        }
        // `skills[slug]` present  => skill exists, value is its
        // required_frontmatter list; absent => not an indexed skill.
        let skills = self.resolve_skills(&wanted).await?;

        // Every instance needs an `id:`. Without one the page indexes and
        // lists, but `expand` fails with `invalid type: null, expected a
        // string` and `resolve` cannot find it — a page that exists and is
        // unreachable. Observed on a real tenant.
        if parsed.frontmatter.page_type == PageType::Instance
            && fields
                .get("id")
                .and_then(YamlValue::as_str)
                .is_none_or(str::is_empty)
        {
            issues.push(Issue::error(
                "frontmatter_required_key_missing",
                "frontmatter.id",
                "an instance page requires a non-empty `id`",
            ));
        }

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

        // Wikilink syntax + referenced-skill existence, over body and
        // frontmatter alike.
        for wl in body_links.iter().chain(fm_links.iter().map(|(_, wl)| wl)) {
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

        // Dangling targets, graded by where the link sits.
        //
        // A link in a REQUIRED frontmatter field is part of the contract:
        // an `offer` whose `customer:` names nothing is the hallucinated-
        // customer case, and the one nobody re-checks. That is an error.
        //
        // Everywhere else it is a warning. Forward references are
        // legitimate in a second brain and the tenant depends on them — a
        // meeting's `continues:` is written pointing at the earlier session
        // before that page exists, and seed scripts cite targets they are
        // about to create. Blocking those would break real workflows to
        // catch a mistake the required-field rule already catches.
        let required_keys: &[String] = declared_skill
            .and_then(|s| skills.get(s))
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        let mut targets: HashSet<(&str, &str)> = HashSet::new();
        for wl in body_links.iter().chain(fm_links.iter().map(|(_, wl)| wl)) {
            if let (Some(skill), Some(id)) = (&wl.skill, &wl.id)
                && skills.contains_key(skill.as_str())
            {
                targets.insert((skill.as_str(), id.as_str()));
            }
        }
        let live = self.resolve_instance_targets(&targets).await?;

        for (key, wl) in &fm_links {
            let (Some(skill), Some(id)) = (&wl.skill, &wl.id) else {
                continue;
            };
            if !skills.contains_key(skill.as_str()) || live.contains(&(skill.clone(), id.clone())) {
                continue;
            }
            let msg = format!("wikilink `[[{skill}::{id}]]` resolves to no page");
            let loc = format!("frontmatter.{key}");
            if required_keys.iter().any(|k| k == key) {
                issues.push(Issue::error("dangling_wikilink", loc, msg));
            } else {
                issues.push(Issue::warning("dangling_wikilink", loc, msg));
            }
        }
        for wl in &body_links {
            let (Some(skill), Some(id)) = (&wl.skill, &wl.id) else {
                continue;
            };
            if !skills.contains_key(skill.as_str()) || live.contains(&(skill.clone(), id.clone())) {
                continue;
            }
            issues.push(Issue::warning(
                "dangling_wikilink",
                format!("wikilink `[[{skill}::{id}]]`"),
                format!("wikilink `[[{skill}::{id}]]` resolves to no page"),
            ));
        }

        Ok(issues)
    }

    /// Which of `targets` exist as indexed instance pages, in one locked
    /// pass. Keyed `(skill, id)`; absent means dangling.
    async fn resolve_instance_targets(
        &self,
        targets: &HashSet<(&str, &str)>,
    ) -> Result<HashSet<(String, String)>, IndexerError> {
        let mut out = HashSet::new();
        if targets.is_empty() {
            return Ok(out);
        }
        let placeholders = std::iter::repeat_n("?", targets.len())
            .collect::<Vec<_>>()
            .join(", ");
        // Match on slug and carry the skill back so two skills sharing a
        // slug cannot vouch for one another.
        let sql = format!(
            "SELECT skill, slug FROM pages \
             WHERE page_type = 'instance' AND slug IN ({placeholders})"
        );
        let bindings: Vec<String> = targets.iter().map(|(_, id)| (*id).to_owned()).collect();

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn duckdb::ToSql> =
            bindings.iter().map(|b| b as &dyn duckdb::ToSql).collect();
        let mut rows = stmt.query(param_refs.as_slice())?;
        while let Some(row) = rows.next()? {
            let skill: Option<String> = row.get(0)?;
            let slug: String = row.get(1)?;
            if let Some(skill) = skill {
                out.insert((skill, slug));
            }
        }
        Ok(out)
    }

    /// Typed wikilinks appearing in frontmatter *values*, paired with the
    /// key they sit under so a required-field link can be graded.
    fn frontmatter_wikilinks(fields: &YamlMapping) -> Vec<(String, WikilinkParsed)> {
        let mut out = Vec::new();
        for (key, value) in fields.iter() {
            let Some(key) = key.as_str() else { continue };
            let mut texts: Vec<&str> = Vec::new();
            match value {
                YamlValue::String(s) => texts.push(s),
                YamlValue::Sequence(items) => {
                    for item in items {
                        if let YamlValue::String(s) = item {
                            texts.push(s);
                        }
                    }
                }
                _ => {}
            }
            for text in texts {
                for wl in parse_wikilinks(text) {
                    out.push((key.to_owned(), wl));
                }
            }
        }
        out
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
        // Overlay-shadows-base determinism (REQ-LAYER-03): with a shadow
        // pair both rows match; base rows sort FIRST so the overlay's
        // frontmatter overwrites it in the map below (last write wins).
        let sql = format!(
            "SELECT slug, frontmatter::VARCHAR FROM pages \
             WHERE page_type = 'skill' AND slug IN ({placeholders}) \
             ORDER BY (page_id LIKE 'markdown/base/%') DESC"
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
