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
use escurel_md::{PageKind, YamlMapping, YamlValue, parse};

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
/// The longest `summary:` the workbench renders in a skill list (workbench
/// backend P2-7). Longer belongs in the body.
pub const SUMMARY_MAX_CHARS: usize = 200;

/// The harness adapters a skill may name (`harness:`). Kept in lock-step
/// with the runner's `build_harness_named`; a name outside it is a lint
/// finding here and a refused run there.
pub const KNOWN_HARNESSES: [&str; 7] = [
    "echo", "claude", "codex", "agy", "muse", "gemini", "delegate",
];

/// `summary:` on a skill page (workbench backend P2-7): absent is a
/// warning (the workbench falls back to `description`), over
/// [`SUMMARY_MAX_CHARS`] is an error.
fn check_summary(page_type: PageKind, fields: &YamlMapping) -> Option<Issue> {
    if page_type != PageKind::Skill {
        return None;
    }
    let text = fields
        .get("summary")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match text {
        None => Some(
            Issue::warning(
                "summary_missing",
                "frontmatter.summary",
                "a skill declares a one-line `summary:` for skill lists (the workbench \
                 shows `description` until it does)",
            )
            .with_suggestion("add `summary: <one line, under 200 characters>`"),
        ),
        Some(s) if s.chars().count() > SUMMARY_MAX_CHARS => Some(Issue::error(
            "summary_too_long",
            "frontmatter.summary",
            format!(
                "`summary:` is {} characters; at most {SUMMARY_MAX_CHARS} (the rest belongs in the body)",
                s.chars().count()
            ),
        )),
        Some(_) => None,
    }
}

/// `harness:` on a skill page names an adapter the runner has.
fn check_harness(page_type: PageKind, fields: &YamlMapping) -> Option<Issue> {
    if page_type != PageKind::Skill {
        return None;
    }
    let raw = fields.get("harness")?;
    let suggestion = format!("use one of: {}", KNOWN_HARNESSES.join(" | "));
    match raw.as_str().map(str::trim) {
        Some(name) if KNOWN_HARNESSES.contains(&name) => None,
        Some(name) => Some(
            Issue::error(
                "harness_unknown",
                "frontmatter.harness",
                format!("`harness: {name}` names no harness adapter"),
            )
            .with_suggestion(suggestion),
        ),
        None => Some(
            Issue::error(
                "harness_unknown",
                "frontmatter.harness",
                "`harness:` must be a string naming the adapter",
            )
            .with_suggestion(suggestion),
        ),
    }
}

/// The roles a skill may declare (`role:`), as a knowledge tree sorts and icons them.
pub const SKILL_ROLES: [&str; 4] = ["record", "process", "report", "helper"];

/// `folder:` on a skill page: a `/`-separated path of slugs (`sales/orders`). A malformed one is an
/// error: a tree cannot place the skill.
fn check_folder(page_type: PageKind, fields: &YamlMapping) -> Option<Issue> {
    if page_type != PageKind::Skill {
        return None;
    }
    let raw = fields.get("folder")?;
    let suggestion = "a `/`-separated path of lowercase slugs, e.g. `folder: sales/orders`";
    let Some(path) = raw.as_str().map(str::trim) else {
        return Some(
            Issue::error(
                "folder_invalid",
                "frontmatter.folder",
                "`folder:` must be a string path",
            )
            .with_suggestion(suggestion),
        );
    };
    if path.is_empty() || !path.split('/').all(is_action_slug) {
        return Some(
            Issue::error(
                "folder_invalid",
                "frontmatter.folder",
                format!("`folder: {path}` is not a `/`-separated path of lowercase slugs (letters, digits, `-`, `_`)"),
            )
            .with_suggestion(suggestion),
        );
    }
    None
}

/// `role:` on a skill page: one of [`SKILL_ROLES`].
fn check_role(page_type: PageKind, fields: &YamlMapping) -> Option<Issue> {
    if page_type != PageKind::Skill {
        return None;
    }
    let raw = fields.get("role")?;
    match raw.as_str().map(str::trim) {
        Some(role) if SKILL_ROLES.contains(&role) => None,
        other => Some(
            Issue::error(
                "role_unknown",
                "frontmatter.role",
                format!(
                    "`role: {}` is not a skill role",
                    other.unwrap_or("<not a string>")
                ),
            )
            .with_suggestion(format!("use one of: {}", SKILL_ROLES.join(" | "))),
        ),
    }
}

/// Whether `s` is an ISO-8601 duration such as `P90D`, `P1Y2M`, `PT36H` or `P1W`.
fn is_iso_duration(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('P') else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let (date_part, time_part) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    let units = |part: &str, allowed: &str| -> bool {
        let mut digits = 0;
        let mut seen = 0;
        for c in part.chars() {
            if c.is_ascii_digit() {
                digits += 1;
            } else if allowed.contains(c) && digits > 0 {
                digits = 0;
                seen += 1;
            } else {
                return false;
            }
        }
        digits == 0 && (seen > 0 || part.is_empty())
    };
    if !units(date_part, "YMWD") {
        return false;
    }
    match time_part {
        Some(t) => !t.is_empty() && units(t, "HMS"),
        None => !date_part.is_empty(),
    }
}

/// The OKF keys on a SKILL page (`tags`, `generated`, `verified`, `stale_after`, `sources`): all
/// optional, and a malformed one is a WARNING, never an error. Unknown keys are never looked at.
/// (`title`, `resource` and `status` are free text; `status` keeps whatever meaning the skill gives it.)
fn check_okf_keys(page_type: PageKind, fields: &YamlMapping) -> Vec<Issue> {
    let mut issues = Vec::new();
    if page_type != PageKind::Skill {
        return issues;
    }
    let warn = |code: &str, key: &str, msg: String, suggestion: &str| {
        Issue::warning(code, format!("frontmatter.{key}"), msg).with_suggestion(suggestion)
    };
    if let Some(v) = fields.get("tags") {
        let ok = v
            .as_sequence()
            .is_some_and(|seq| seq.iter().all(|t| t.as_str().is_some()));
        if !ok {
            issues.push(warn(
                "tags_invalid",
                "tags",
                "`tags:` is a list of strings".to_owned(),
                "write `tags: [sales, sap]`",
            ));
        }
    }
    if let Some(v) = fields.get("sources") {
        let ok = v.as_sequence().is_some_and(|seq| {
            seq.iter()
                .all(|t| t.as_str().is_some() || t.as_mapping().is_some())
        });
        if !ok {
            issues.push(warn(
                "sources_invalid",
                "sources",
                "`sources:` is a list (of links or `{title, url}` entries)".to_owned(),
                "write `sources: [https://example.com/doc]`",
            ));
        }
    }
    for key in ["generated", "verified"] {
        if let Some(v) = fields.get(key) {
            let ok = v
                .as_str()
                .map(str::trim)
                .is_some_and(|t| is_date(t) || is_datetime(t));
            if !ok {
                issues.push(warn(
                    &format!("{key}_invalid"),
                    key,
                    format!("`{key}:` is a date (`YYYY-MM-DD`) or an RFC 3339 timestamp"),
                    "e.g. `2026-10-01T10:00:00Z`",
                ));
            }
        }
    }
    if let Some(v) = fields.get("stale_after") {
        let ok = v
            .as_str()
            .map(str::trim)
            .is_some_and(|t| is_date(t) || is_datetime(t) || is_iso_duration(t));
        if !ok {
            issues.push(warn(
                "stale_after_invalid",
                "stale_after",
                "`stale_after:` is an RFC 3339 instant or an ISO-8601 duration".to_owned(),
                "e.g. `2027-01-01T00:00:00Z` or `P90D`",
            ));
        }
    }
    issues
}

/// A slug as an action `name` takes: lowercase letters, digits, `-` and `_`.
fn is_action_slug(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Lints a skill's `actions:` (Peacock's object form). Returns the findings and the
/// `(index, skill)` of every `kind: event` entry, whose skill the caller checks against the corpus.
fn check_actions(raw: &YamlValue) -> (Vec<Issue>, Vec<(usize, String)>) {
    let mut issues = Vec::new();
    let mut events = Vec::new();
    let Some(seq) = raw.as_sequence() else {
        issues.push(Issue::error(
            "action_invalid",
            "frontmatter.actions",
            "`actions:` must be a list of action objects: `{name, kind: event|prompt, label, event|prompt}`",
        ));
        return (issues, events);
    };
    let mut seen: HashSet<String> = HashSet::new();
    let text = |m: &YamlMapping, k: &str| -> Option<String> {
        m.get(k)
            .and_then(YamlValue::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    for (i, entry) in seq.iter().enumerate() {
        let at = |field: &str| format!("frontmatter.actions[{i}].{field}");
        let Some(m) = entry.as_mapping() else {
            issues.push(Issue::error(
                "action_invalid",
                format!("frontmatter.actions[{i}]"),
                "an `actions` entry is an object `{name, kind: event|prompt, label, event|prompt}`, not a bare skill id",
            ));
            continue;
        };
        match text(m, "name") {
            Some(name) if is_action_slug(&name) => {
                if !seen.insert(name.clone()) {
                    issues.push(Issue::error(
                        "action_name_duplicate",
                        at("name"),
                        format!("two actions are named `{name}`; a name is the action's id"),
                    ));
                }
            }
            _ => issues.push(Issue::error(
                "action_name_invalid",
                at("name"),
                "`name` is a slug: lowercase letters, digits, `-` and `_`",
            )),
        }
        if text(m, "label").is_none() {
            issues.push(Issue::error(
                "action_label_missing",
                at("label"),
                "`label` is the text of the button",
            ));
        }
        match text(m, "kind").as_deref() {
            Some("event") => match text(m, "event") {
                Some(skill) => events.push((i, skill)),
                None => issues.push(Issue::error(
                    "action_event_missing",
                    at("event"),
                    "a `kind: event` action names the skill its event is filed under",
                )),
            },
            Some("prompt") => {
                if text(m, "prompt").is_none() {
                    issues.push(Issue::error(
                        "action_prompt_missing",
                        at("prompt"),
                        "a `kind: prompt` action carries the `prompt` text",
                    ));
                }
            }
            _ => issues.push(Issue::error(
                "action_kind_unknown",
                at("kind"),
                "`kind` is `event` or `prompt`",
            )),
        }
    }
    (issues, events)
}

fn check_autonomy(page_type: PageKind, fields: &YamlMapping) -> Option<Issue> {
    if page_type != PageKind::Skill {
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
fn check_params(page_type: PageKind, fields: &YamlMapping) -> Vec<Issue> {
    if page_type != PageKind::Skill {
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

/// The skill author's own `fields:` block (#508) — checked on the SKILL page,
/// so a malformed schema is reported once to the person who wrote it rather
/// than on every instance of it.
///
/// Mirrors [`check_params`] exactly, including the fallback direction: an
/// unknown `kind:` is a WARNING and the field degrades to `string`, because an
/// over-permissive field under-validates while a dropped one silently deletes
/// a constraint the author believes is in force.
fn check_fields(page_type: PageKind, fields: &YamlMapping) -> Vec<Issue> {
    if page_type != PageKind::Skill {
        return Vec::new();
    }
    let Some(raw) = fields.get("fields") else {
        return Vec::new();
    };
    let recognised: Vec<&str> = crate::FieldKind::recognised()
        .iter()
        .map(|k| k.as_str())
        .collect();
    let suggestion = format!("use one of: {}", recognised.join(" | "));
    let malformed = |message: &str| {
        vec![
            Issue::error("fields_malformed", "frontmatter.fields", message)
                .with_suggestion("e.g. `- {name: hotness, kind: enum, values: [hot, warm, cold]}`"),
        ]
    };

    // (name, declared kind, declared values, declared render) per entry.
    type Entry<'a> = (
        String,
        Option<&'a YamlValue>,
        Option<&'a YamlValue>,
        Option<&'a YamlValue>,
    );
    let entries: Vec<Entry<'_>> = if let Some(seq) = raw.as_sequence() {
        let mut out = Vec::new();
        for item in seq {
            let m = item.as_mapping();
            let Some(name) = m.and_then(|m| m.get("name")).and_then(YamlValue::as_str) else {
                return malformed(
                    "every `fields:` entry must be a mapping with a `name:` — a \
                     field with no name constrains no frontmatter key",
                );
            };
            out.push((
                name.to_owned(),
                m.and_then(|m| m.get("kind").or_else(|| m.get("type"))),
                m.and_then(|m| m.get("values")),
                m.and_then(|m| m.get("render")),
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
                    attrs.and_then(|m| m.get("values")),
                    attrs.and_then(|m| m.get("render")),
                ))
            })
            .collect()
    } else {
        return malformed(
            "`fields:` must be a sequence of `{name, kind, …}` entries or a \
             mapping of name to those attributes",
        );
    };

    let mut issues = Vec::new();
    for (name, kind, values, render) in entries {
        // `render:` is a hint a client switches on; an unknown one is passed
        // through and ignored there, so this is a warning, not a refusal
        // (workbench backend P3-5).
        if let Some(r) = render
            && !r
                .as_str()
                .is_some_and(|r| KNOWN_RENDERS.contains(&r.trim()))
        {
            let shown = r.as_str().map_or_else(|| format!("{r:?}"), str::to_owned);
            issues.push(
                Issue::warning(
                    "field_render_unknown",
                    format!("frontmatter.fields.{name}.render"),
                    format!(
                        "`render: {shown}` on field `{name}` is not a render hint a client \
                         knows; it is passed through and ignored"
                    ),
                )
                .with_suggestion(format!("use one of: {}", KNOWN_RENDERS.join(" | "))),
            );
        }
        let parsed = kind
            .and_then(YamlValue::as_str)
            .and_then(crate::FieldKind::parse);
        match (kind, parsed) {
            // No `kind:` at all is not a finding: an undeclared kind is a text
            // field, which is what an author who omitted it meant.
            (None, _) | (Some(_), Some(_)) => {}
            (Some(declared), None) => {
                let shown = declared
                    .as_str()
                    .map_or_else(|| format!("{declared:?}"), str::to_owned);
                issues.push(
                    Issue::warning(
                        "field_kind_unknown",
                        format!("frontmatter.fields.{name}.kind"),
                        format!(
                            "`kind: {shown}` on field `{name}` is not a recognised kind; \
                             it is enforced as `string`, which constrains nothing"
                        ),
                    )
                    .with_suggestion(suggestion.clone()),
                );
            }
        }
        // An enum with no values constrains nothing, which is never what the
        // author meant by writing `kind: enum` — and it fails OPEN, so it is
        // exactly the kind of mistake nobody notices from the outside.
        if parsed == Some(crate::FieldKind::Enum)
            && values
                .and_then(YamlValue::as_sequence)
                .is_none_or(|v| v.is_empty())
        {
            issues.push(
                Issue::error(
                    "fields_malformed",
                    format!("frontmatter.fields.{name}.values"),
                    format!(
                        "field `{name}` is `kind: enum` with no `values:` — an enum \
                         with no members accepts everything, so nothing is enforced"
                    ),
                )
                .with_suggestion("values: [hot, warm, cold]"),
            );
        }
    }
    issues
}

/// The render hints the workbench knows (workbench backend P3-5).
const KNOWN_RENDERS: &[&str] = &[
    "text", "markdown", "date", "datetime", "money", "link", "badge",
];

/// `blocks:` on a skill page is a sequence of `{anchor, title?, kind?}`
/// mappings (workbench backend P3-5). A block without an anchor has nowhere
/// to render, so that — and a `blocks:` that is not a sequence — is an error
/// at the offending location.
fn check_blocks(page_type: PageKind, fields: &YamlMapping) -> Vec<Issue> {
    if page_type != PageKind::Skill {
        return Vec::new();
    }
    let Some(raw) = fields.get("blocks") else {
        return Vec::new();
    };
    let suggestion = "e.g. `- {anchor: summary, title: Summary, kind: markdown}`";
    let Some(seq) = raw.as_sequence() else {
        return vec![
            Issue::error(
                "blocks_malformed",
                "frontmatter.blocks",
                "`blocks:` must be a sequence of `{anchor, title?, kind?}` entries",
            )
            .with_suggestion(suggestion),
        ];
    };
    seq.iter()
        .enumerate()
        .filter(|(_, item)| {
            item.as_mapping()
                .and_then(|m| m.get("anchor"))
                .and_then(YamlValue::as_str)
                .is_none_or(|a| a.trim().is_empty())
        })
        .map(|(i, _)| {
            Issue::error(
                "blocks_malformed",
                format!("frontmatter.blocks[{i}]"),
                "every `blocks:` entry must be a mapping with a non-empty `anchor:` — a \
                 block with no anchor has nowhere to render",
            )
            .with_suggestion(suggestion)
        })
        .collect()
}

/// Check one instance frontmatter value against the field its skill declared
/// (#508). Returns the issues for that key — none when it fits.
///
/// Values arrive as YAML, so the check is on the PARSED shape rather than on
/// text: `seats: 12` is already an integer, `opened: 2026-01-05` is already a
/// date to serde_yaml, and `active: yes` is already a bool. A value that YAML
/// gave us as a string is re-parsed from its text, which is how a quoted
/// `"12"` still satisfies `kind: int` — the author's quoting habit is not a
/// type error.
fn check_field_value(field: &crate::SkillField, value: &YamlValue) -> Vec<Issue> {
    use crate::FieldKind;
    let location = format!("frontmatter.{}", field.name);
    let name = &field.name;
    // The value as the author would recognise it: a YAML string as itself, a
    // scalar through its JSON spelling (`12`, `true`) rather than Rust's Debug.
    let shown = || match value.as_str() {
        Some(s) => s.to_owned(),
        None => serde_json::to_value(value)
            .ok()
            .map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
            .unwrap_or_else(|| format!("{value:?}")),
    };
    let type_error = |expected: &str| {
        vec![Issue::error(
            "frontmatter_field_type",
            location.clone(),
            format!(
                "`{name}: {}` does not parse as {expected}, which is what skill \
                 declares for this field",
                shown()
            ),
        )]
    };

    // A number, however the author wrote it.
    let as_number = || -> Option<f64> {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    };
    let as_integer = || -> Option<i64> {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    };

    let mut issues = match field.kind {
        // A string constrains nothing by itself — that is what `string` means.
        FieldKind::String | FieldKind::Link => Vec::new(),
        FieldKind::Integer => match as_integer() {
            Some(_) => Vec::new(),
            None => type_error("a whole number"),
        },
        FieldKind::Float => match as_number() {
            Some(_) => Vec::new(),
            None => type_error("a number"),
        },
        FieldKind::Boolean => {
            let ok = value.as_bool().is_some()
                || value.as_str().is_some_and(|s| {
                    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "false")
                });
            if ok {
                Vec::new()
            } else {
                type_error("a boolean (`true` / `false`)")
            }
        }
        FieldKind::Date => {
            if is_date(&shown()) {
                Vec::new()
            } else {
                type_error("a date (`YYYY-MM-DD`)")
            }
        }
        FieldKind::DateTime => {
            let text = shown();
            if is_date(&text) || is_datetime(&text) {
                Vec::new()
            } else {
                type_error("a timestamp (`YYYY-MM-DDTHH:MM:SSZ`)")
            }
        }
        FieldKind::Enum => {
            let text = shown();
            if field.values.iter().any(|v| v == &text) {
                Vec::new()
            } else {
                vec![
                    Issue::error(
                        "frontmatter_enum_value",
                        location.clone(),
                        format!(
                            "`{name}: {text}` is not one of the declared values: {}",
                            field.values.join(", ")
                        ),
                    )
                    // Naming the allowed set in the suggestion too, because an
                    // agent reading only `suggestion` still gets the answer.
                    .with_suggestion(format!("{name}: {}", field.values.join(" | "))),
                ]
            }
        }
    };

    // Bounds apply to whatever parsed as a number, whichever numeric kind was
    // declared. A bound on a non-numeric field is the author's mistake and is
    // simply inert — reporting it on every instance would be noise aimed at
    // the wrong person.
    if issues.is_empty()
        && let Some(n) = as_number()
    {
        if let Some(min) = field.min
            && n < min
        {
            issues.push(Issue::error(
                "frontmatter_field_range",
                location.clone(),
                format!("`{name}: {n}` is below the declared minimum {min}"),
            ));
        }
        if let Some(max) = field.max
            && n > max
        {
            issues.push(Issue::error(
                "frontmatter_field_range",
                location,
                format!("`{name}: {n}` is above the declared maximum {max}"),
            ));
        }
    }
    issues
}

/// `YYYY-MM-DD`, the shape `at:` is already indexed from.
fn is_date(text: &str) -> bool {
    let t = text.trim();
    let b = t.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// A date followed by a time — `T`-separated or space-separated, with or
/// without a zone. Deliberately shape-only: escurel stores what the author
/// wrote and DuckDB does the real parsing at index time.
fn is_datetime(text: &str) -> bool {
    let t = text.trim();
    let Some((date, time)) = t.split_once(['T', ' ']) else {
        return false;
    };
    is_date(date)
        && time.len() >= 5
        && time.as_bytes()[2] == b':'
        && time[..2].bytes().all(|c| c.is_ascii_digit())
}

/// What a skill demands of its instances: which keys must be present, and —
/// when the skill declares `fields:` (#508) — what shape their values take.
#[derive(Debug, Clone, Default)]
struct SkillContract {
    required: Vec<String>,
    fields: Vec<crate::SkillField>,
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
            Err(escurel_md::ParseError::LegacyTypeKey) => {
                // Not a YAML error: the page is fine and uses the REMOVED page-kind key. Say which
                // key, and how a tenant's stored pages are rewritten.
                let mut issue = Issue::error(
                    "frontmatter_type_removed",
                    "frontmatter.type",
                    "the `type: skill|instance` page-kind key was removed; the page kind is `kind:` now",
                );
                issue.suggestion = Some(
                    "rename `type:` to `kind:` in this page; rewrite a tenant's stored pages with \
                     `escurel admin migrate-kind`"
                        .to_owned(),
                );
                return Ok(vec![issue]);
            }
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
        issues.extend(check_autonomy(parsed.frontmatter.page_kind, fields));
        // The workbench's skill-contract keys (P2-7): the one-liner and the
        // adapter, both local; the fan-out list needs the corpus (below).
        issues.extend(check_summary(parsed.frontmatter.page_kind, fields));
        issues.extend(check_harness(parsed.frontmatter.page_kind, fields));
        // Where a skill sits and what it is (OKF-aligned tree vocabulary), then the optional OKF keys.
        issues.extend(check_folder(parsed.frontmatter.page_kind, fields));
        issues.extend(check_role(parsed.frontmatter.page_kind, fields));
        issues.extend(check_okf_keys(parsed.frontmatter.page_kind, fields));
        if parsed.frontmatter.page_kind == PageKind::Skill
            && let Some(raw) = fields.get("actions")
        {
            let (action_issues, events) = check_actions(raw);
            issues.extend(action_issues);
            let wanted: HashSet<&str> = events.iter().map(|(_, n)| n.as_str()).collect();
            let known = self.resolve_skills(&wanted).await?;
            for (i, name) in events {
                if !known.contains_key(&name) {
                    issues.push(Issue::error(
                        "action_skill_unknown",
                        format!("frontmatter.actions[{i}].event"),
                        format!("`event` names skill `{name}`, which this corpus does not have"),
                    ));
                }
            }
        }
        // The invocation-parameter block a skill declares (heron#11 / CR-7).
        issues.extend(check_params(parsed.frontmatter.page_kind, fields));
        // The instance-shape block a skill declares (#508). Checked on the
        // SKILL page, so a malformed schema reaches its author once rather
        // than every instance's author repeatedly.
        issues.extend(check_fields(parsed.frontmatter.page_kind, fields));
        // The instance-body layout a skill declares (workbench P3-5).
        issues.extend(check_blocks(parsed.frontmatter.page_kind, fields));
        // A stored corpus traversal (#511). Checked HERE rather than at query
        // time: a bound that is only enforced when someone runs the query is a
        // bound that ships broken, and the author finds out from a stranger.
        issues.extend(self.check_traversal(&parsed.frontmatter).await?);

        // Skill pages declare themselves via `id:`; instance pages
        // via `skill:`.
        let declared_skill = match parsed.frontmatter.page_kind {
            PageKind::Instance => fields.get("skill").and_then(YamlValue::as_str),
            PageKind::Skill => fields.get("id").and_then(YamlValue::as_str),
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
            if let (Some(skill), Some(id)) = (&wl.skill, &wl.id) {
                // The reserved `skill::` namespace names a skill DEFINITION
                // page, so the skill to look up is the link's ID segment.
                // Fetching `skill` instead asks for a skill nobody has, which
                // is what made `[[skill::<id>]]` unwritable (#424).
                wanted.insert(if skill == "skill" {
                    id.as_str()
                } else {
                    skill.as_str()
                });
            }
        }
        // `skills[slug]` present  => skill exists, value is its
        // required_frontmatter list; absent => not an indexed skill.
        let skills = self.resolve_skills(&wanted).await?;

        // Every instance needs an `id:`. Without one the page indexes and
        // lists, but `expand` fails with `invalid type: null, expected a
        // string` and `resolve` cannot find it — a page that exists and is
        // unreachable. Observed on a real tenant.
        if parsed.frontmatter.page_kind == PageKind::Instance
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

        // An instance with no `skill:` is a typed page with no type.
        //
        // It indexes, it expands, its wikilinks resolve — and
        // `list_instances` cannot find it, because that is the query that
        // reads this field. So the page is real, linked, and invisible to
        // every catalogue view a reader actually browses.
        //
        // Found end to end: an agent drafted a `note` under
        // `markdown/instances/note/…`, a human approved it, the `about:`
        // edge into the customer was there — and the note was not in
        // `list_instances --skill note`, so it could never appear in the
        // client's Browse. The page id LOOKS like it declares the skill and
        // does not.
        //
        // Symmetric with the `id` rule above, and for the same reason: both
        // are identity failures rather than completeness ones.
        if parsed.frontmatter.page_kind == PageKind::Instance
            && fields
                .get("skill")
                .and_then(YamlValue::as_str)
                .is_none_or(str::is_empty)
        {
            issues.push(Issue::error(
                "frontmatter_required_key_missing",
                "frontmatter.skill",
                "an instance page requires a non-empty `skill` — without it the \
                 page indexes but `list_instances` cannot find it, so no \
                 catalogue view will ever show it",
            ));
        }

        for (key, text) in Self::unquoted_frontmatter_wikilinks(fields) {
            issues.push(
                Issue::warning(
                    "frontmatter_wikilink_unquoted",
                    format!("frontmatter.{key}"),
                    format!(
                        "`{key}: [[{text}]]` parses as a nested YAML list, not a string \
                         — the link edge still resolves, but any consumer reading this \
                         field as a value sees a list"
                    ),
                )
                .with_suggestion(format!("{key}: \"[[{text}]]\"")),
            );
        }

        // required_frontmatter — only when the draft's declared
        // skill resolves to a skill page that declares required keys.
        //
        // INSTANCES only. `required_frontmatter` describes what a skill's
        // instances must carry, not what the skill page itself does, and
        // `declared_skill` resolves to a skill page's OWN id — so applying it
        // here made every skill fail its own rule. Measured while seeding the
        // deployed corpus: `markdown/skills/calendar.md` was reported as
        // missing `at`, `source` and `channel`, which are the fields a
        // calendar ENTRY has. The write path accepts the page (it blocks only
        // a missing `id`/`skill`), so `page validate` said REJECTED about
        // content `page update` then wrote — a dry run that disagrees with
        // the real thing is worse than no dry run, because it teaches people
        // to ignore it.
        if let Some(skill) = declared_skill
            && parsed.frontmatter.page_kind == PageKind::Instance
        {
            match skills.get(skill) {
                // A `skill:` on an instance that names a non-existent
                // skill is itself an unknown-skill error.
                None if parsed.frontmatter.page_kind == PageKind::Instance => {
                    issues.push(Issue::error(
                        "unknown_skill",
                        "frontmatter.skill",
                        format!("declared skill `{skill}` is not an indexed skill page"),
                    ));
                }
                Some(contract) => {
                    // A ROW of an `instances: rows` skill gets its projected columns from the source, so its
                    // linked markdown need not (and may not) carry them: they are never "missing".
                    let source_supplied: Vec<String> = match self.rows_source(skill).await {
                        Ok(Some(src)) => src.project.values().cloned().collect(),
                        _ => Vec::new(),
                    };
                    for key in &contract.required {
                        if fields.get(key.as_str()).is_none() && !source_supplied.contains(key) {
                            issues.push(Issue::error(
                                "frontmatter_required_key_missing",
                                format!("frontmatter.{key}"),
                                format!("skill `{skill}` requires frontmatter key `{key}`"),
                            ));
                        }
                    }

                    // Typed fields (#508). `required_frontmatter` says a key
                    // must be THERE; `fields:` says what may be IN it — the
                    // difference between a corpus you can filter and one that
                    // has quietly fractured into synonym classes.
                    //
                    // A field's `required:` is reported with the SAME code as
                    // a missing `required_frontmatter` key: a reviewer should
                    // not have to learn two vocabularies for one missing key.
                    for field in &contract.fields {
                        match fields.get(field.name.as_str()) {
                            Some(value) => issues.extend(check_field_value(field, value)),
                            None if field.required
                                && !source_supplied.contains(&field.name)
                                && !contract.required.iter().any(|k| k == &field.name) =>
                            {
                                issues.push(Issue::error(
                                    "frontmatter_required_key_missing",
                                    format!("frontmatter.{}", field.name),
                                    format!(
                                        "skill `{skill}` declares `{}` as a required field",
                                        field.name
                                    ),
                                ));
                            }
                            None => {}
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
                (Some(skill), Some(id)) => {
                    // The reserved `skill::` namespace (#212): `[[skill::<id>]]`
                    // names a skill DEFINITION page, so the thing that must
                    // exist is the skill `<id>` — not a skill called `skill`,
                    // which never exists and made every use of the documented
                    // form unwritable (#424). `resolve` has always constrained
                    // this on `page_type = 'skill'`; the validator did not
                    // know, so a page that resolved could not be saved.
                    //
                    // Heron found it: its workshop formats reference a shared
                    // procedure exactly this way (BR-WS-2), and its Rust
                    // fixtures write straight to the store, so validation
                    // never ran on them until an app test authored a format
                    // through the real write path.
                    let (missing, named) = if skill == "skill" {
                        (!skills.contains_key(id.as_str()), id)
                    } else {
                        (!skills.contains_key(skill.as_str()), skill)
                    };
                    if missing {
                        issues.push(Issue::error(
                            "unknown_skill",
                            format!("wikilink `[[{skill}::...]]`"),
                            format!("wikilink references unknown skill `{named}`"),
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
            .map(|c| c.required.as_slice())
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
    /// Frontmatter fields holding an UNQUOTED wikilink.
    ///
    /// `about: [[customer::acme]]` is not a string in YAML. It is a sequence
    /// containing a sequence containing `customer::acme`.
    ///
    /// **The edge still resolves.** That was measured, not assumed, and it
    /// is the opposite of what this check was first written to claim: two
    /// otherwise identical pages, one quoted and one not, both produce the
    /// in-edge on `neighbours`. Edge extraction reads the raw frontmatter
    /// text, so the YAML shape does not reach it.
    ///
    /// What the shape DOES change is every consumer that reads the field as
    /// a value rather than as text — the parsed frontmatter a client
    /// receives holds `[["customer::acme"]]`, not a string. Heron reads
    /// `engagement:` that way, and a field that silently becomes a nested
    /// list is a defect waiting for the first consumer who reads it.
    ///
    /// So: a WARNING, not an error. Nothing is lost today; the value is not
    /// what its author wrote.
    ///
    /// Detected structurally rather than by re-parsing text: a one-element
    /// sequence whose one element is a one-element sequence of a string
    /// shaped `<skill>::<id>` is a YAML flow list nobody writes on purpose.
    fn unquoted_frontmatter_wikilinks(fields: &YamlMapping) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (key, value) in fields.iter() {
            let Some(key) = key.as_str() else { continue };
            let YamlValue::Sequence(outer) = value else {
                continue;
            };
            for item in outer {
                let YamlValue::Sequence(inner) = item else {
                    continue;
                };
                for leaf in inner {
                    if let YamlValue::String(text) = leaf
                        && let Some((skill, id)) = text.split_once("::")
                        && !skill.is_empty()
                        && !id.is_empty()
                        && skill
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        out.push((key.to_owned(), text.clone()));
                    }
                }
            }
        }
        out
    }

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

    /// Check a `target: corpus` query page's `traversal:` block (#511).
    ///
    /// Every structural defect is an error — a traversal that cannot be
    /// compiled cannot be run, so reporting it as a warning would only move
    /// the failure to whoever calls it next. An unknown RELATION is a warning:
    /// it is the one defect that might be a forward reference (the pages that
    /// will carry that key are not written yet), and a corpus is routinely
    /// seeded in an order that makes that true.
    async fn check_traversal(
        &self,
        frontmatter: &escurel_md::Frontmatter,
    ) -> Result<Vec<Issue>, IndexerError> {
        if frontmatter.page_kind != PageKind::Instance
            || frontmatter.fields.get("skill").and_then(YamlValue::as_str) != Some("query")
        {
            return Ok(Vec::new());
        }
        let fm =
            serde_json::to_value(&frontmatter.fields).unwrap_or_else(|_| serde_json::json!({}));
        let is_corpus = fm
            .get("target")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|t| t.trim() == crate::CORPUS_TARGET);
        if !is_corpus {
            // A `sql_view` query page that happens to carry a `traversal:`
            // key is not a traversal, and saying so would be noise about a
            // key this path does not read.
            return Ok(Vec::new());
        }

        let traversal = match crate::parse_traversal(&fm) {
            Ok(Some(t)) => t,
            Ok(None) => {
                return Ok(vec![Issue::error(
                    "traversal_malformed",
                    "frontmatter.traversal",
                    "`target: corpus` but the page declares no `traversal:` block — there \
                     is nothing to run",
                )]);
            }
            Err(e) => {
                return Ok(vec![
                    Issue::error(e.code(), "frontmatter.traversal", e.message().to_owned())
                        .with_suggestion(
                            "see docs/contract/agent-interface.md § stored corpus traversals",
                        ),
                ]);
            }
        };

        // `relation:` is a frontmatter KEY that links were written under. A
        // typo returns an empty result set rather than an error, which is the
        // worst shape of failure — it looks like an answer.
        let mut issues = Vec::new();
        for step in &traversal.steps {
            if !self.relation_is_known(&step.relation).await? {
                issues.push(
                    Issue::warning(
                        "traversal_unknown_relation",
                        format!("frontmatter.traversal.{}", step.relation),
                        format!(
                            "no skill declares `{}` as a `kind: link` field and no link in \
                             this corpus was written under it — a traversal over a relation \
                             nothing uses returns nothing, which reads like an answer",
                            step.relation
                        ),
                    )
                    .with_suggestion(
                        "declare it on the source skill: `fields: [{name: <relation>, \
                         kind: link, target_skill: <skill>}]`",
                    ),
                );
            }
        }
        Ok(issues)
    }

    /// Whether `relation` is a link field anybody declares or any link uses.
    ///
    /// Declared beats observed deliberately: a skill that declares
    /// `kind: link` (#508) has promised the relation exists, and a corpus
    /// seeded skill-pages-first would otherwise warn about every traversal
    /// written before its first instance.
    async fn relation_is_known(&self, relation: &str) -> Result<bool, IndexerError> {
        let conn = self.conn.lock().await;
        let used: i64 = conn.query_row(
            "SELECT count(*) FROM links WHERE src_field = ? LIMIT 1",
            duckdb::params![relation],
            |row| row.get(0),
        )?;
        if used > 0 {
            return Ok(true);
        }
        // Declared as a `kind: link` field on any skill page.
        let declared: i64 = conn.query_row(
            "SELECT count(*) FROM pages WHERE page_type = 'skill' \
             AND frontmatter::VARCHAR LIKE ?",
            duckdb::params![format!("%\"name\":\"{relation}\"%")],
            |row| row.get(0),
        )?;
        Ok(declared > 0)
    }

    /// Resolve a set of skill slugs in a single locked DuckDB pass.
    ///
    /// Returns a map keyed by the slugs that exist as indexed skill
    /// pages (`page_type = 'skill'`); each value is that skill's
    /// [`SkillContract`] — what its instances must CARRY
    /// (`required_frontmatter`) and what shape those values must have
    /// (`fields:`, #508). A slug absent from the map is not an indexed skill —
    /// callers treat that as an `unknown_skill` issue.
    ///
    /// Both halves come from the ONE row already being read, deliberately: the
    /// typed checks must not cost a second pass over the same pages (#508 asks
    /// for exactly this — "fold it into the existing single locked pass").
    async fn resolve_skills(
        &self,
        slugs: &HashSet<&str>,
    ) -> Result<HashMap<String, SkillContract>, IndexerError> {
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
            let fm_json: Option<String> = fm_json;
            let contract = match fm_json {
                Some(s) => {
                    let fm: serde_json::Value = serde_json::from_str(&s)?;
                    SkillContract {
                        required: fm
                            .get("required_frontmatter")
                            .and_then(serde_json::Value::as_array)
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(str::to_owned))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default(),
                        fields: crate::parse_fields(&fm),
                    }
                }
                None => SkillContract::default(),
            };
            out.insert(slug, contract);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod duration_tests {
    use super::is_iso_duration;

    #[test]
    fn accepts_iso_8601_durations_and_nothing_else() {
        for ok in [
            "P90D",
            "P1Y",
            "P1Y2M3D",
            "P2W",
            "PT36H",
            "P1DT12H",
            "PT1H30M5S",
        ] {
            assert!(is_iso_duration(ok), "{ok}");
        }
        for bad in [
            "", "P", "90D", "P90", "PD", "PT", "P1H", "P1DT", "p90d", "P-1D", "P1.5D", "someday",
        ] {
            assert!(!is_iso_duration(bad), "{bad}");
        }
    }
}
