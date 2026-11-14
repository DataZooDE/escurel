//! Markdown parser for Escurel.
//!
//! This crate parses the YAML frontmatter block that every Escurel
//! page (skill or instance) carries, and extracts typed wikilinks
//! from page bodies. See the [`wikilink`] module for the latter.
//!
//! ## Format
//!
//! An Escurel markdown file begins with a YAML frontmatter block
//! delimited by `---` lines, followed by the markdown body:
//!
//! ```text
//! ---
//! kind: skill
//! id: customer
//! description: A buying entity that may have one or more contacts.
//! required_frontmatter: [tier, opened, status]
//! optional_frontmatter: [mrr_band, owner]
//! ---
//!
//! # Customer
//!
//! Body markdown here.
//! ```
//!
//! The required field is `kind:`, which must be `skill` or
//! `instance`. Everything else is preserved verbatim in
//! [`Frontmatter::fields`] for the indexer to project as needed.
//!
//! `kind:` replaced the original `type:` key (OKF alignment, where `type` means the concept's
//! own kind). The legacy key is rejected with [`ParseError::LegacyTypeKey`]; stored pages are
//! rewritten by `escurel admin migrate-kind` (see [`legacy::rewrite_legacy_type_key`]).

pub mod legacy;
pub mod rename;
pub mod skill_md;
pub mod wikilink;

// Re-export the YAML types we expose in our public API so downstream
// crates can read parsed frontmatter without having to depend on
// `serde_yaml_ng` directly.
pub use legacy::{KindRewrite, rewrite_legacy_type_key, rewrite_workflow_run_status};
pub use rename::{
    FieldRename, PageRewrite, PageRewriteOutcome, field_references, rewrite_page_kind,
};
pub use serde_yaml_ng::{Mapping as YamlMapping, Value as YamlValue};

use thiserror::Error;

/// The two kinds of pages an Escurel tenant carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// A type declaration. Defines what its instances look like.
    Skill,
    /// A memory of some skill type.
    Instance,
}

/// Parsed frontmatter for one page.
#[derive(Debug, Clone)]
pub struct Frontmatter {
    /// Convenience projection of the page-kind key (`kind:`).
    pub page_kind: PageKind,
    /// Raw frontmatter mapping (includes `kind` and every other key).
    /// Callers project skill-specific fields from here.
    pub fields: serde_yaml_ng::Mapping,
}

/// A page parsed into its frontmatter + the body slice that follows.
#[derive(Debug, Clone)]
pub struct Page<'a> {
    pub frontmatter: Frontmatter,
    pub body: &'a str,
}

/// Errors returned by [`parse`].
#[derive(Debug, Error)]
pub enum ParseError {
    /// The input did not begin with a `---` line.
    #[error("missing frontmatter: input does not start with '---'")]
    MissingFrontmatter,
    /// A frontmatter block was started but never closed.
    #[error("unterminated frontmatter: no closing '---' found")]
    UnterminatedFrontmatter,
    /// `serde_yaml_ng` could not parse the YAML block.
    #[error("invalid YAML in frontmatter: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    /// Frontmatter parsed as YAML but was not a mapping at the top level.
    #[error("frontmatter must be a YAML mapping at the top level")]
    NotAMapping,
    // REMOVE after v2027.xx together with `legacy.rs`: legacy migration shim (docs/notes/legacy-migration-shims.md).
    /// The page still carries the removed `type: skill|instance` page-kind key. Not a YAML error
    /// and not "no kind at all": the page is fine, it needs migrating.
    #[error(
        "this page uses the removed `type:` page-kind key; it is `kind:` now \
         (run `escurel admin migrate-kind` on the tenant to rewrite stored pages)"
    )]
    LegacyTypeKey,
    /// `kind:` was missing or not `skill` / `instance`.
    #[error("frontmatter missing or invalid 'kind' (expected 'skill' or 'instance')")]
    InvalidType,
    /// An in-place edit ([`set_frontmatter_scalar`]) would change more than the one key.
    #[error("frontmatter key `{0}` cannot be set in place to that value")]
    UnpatchableKey(String),
}

/// Parse a markdown file's frontmatter and split off the body.
///
/// Returns the parsed frontmatter together with a slice referencing
/// the body content (everything after the closing `---` and the
/// following newline).
///
/// # Errors
///
/// Returns [`ParseError`] when the input is missing a frontmatter
/// block, the YAML is malformed, or the required `kind:` field is
/// absent or unrecognised.
pub fn parse(input: &str) -> Result<Page<'_>, ParseError> {
    let (mapping, body) = split_frontmatter(input)?;

    // `kind:` is the page-kind key. The legacy `type: skill|instance` spelling is REMOVED (OKF stage 1
    // hard cut): a page that has it and no valid `kind:` is told so, instead of being reported as a
    // page with no kind at all. A page's own data field named `type` (`type: invoice`) is just data.
    let kind_of = |key: &str| {
        mapping
            .get(key)
            .and_then(serde_yaml_ng::Value::as_str)
            .and_then(|s| match s {
                "skill" => Some(PageKind::Skill),
                "instance" => Some(PageKind::Instance),
                _ => None,
            })
    };
    let page_kind = match kind_of("kind") {
        Some(kind) => kind,
        None if kind_of("type").is_some() => return Err(ParseError::LegacyTypeKey),
        None => return Err(ParseError::InvalidType),
    };

    Ok(Page {
        frontmatter: Frontmatter {
            page_kind,
            fields: mapping,
        },
        body,
    })
}

/// Split a `---`-fenced YAML frontmatter block off a markdown document, without asking what kind of
/// document it is. [`parse`] is this plus the escurel `kind:` rule; a foreign format that shares the
/// fence (an Anthropic `SKILL.md`, which has `name:` and no `kind:`) needs only this half.
///
/// # Errors
///
/// [`ParseError::MissingFrontmatter`], [`ParseError::UnterminatedFrontmatter`],
/// [`ParseError::Yaml`] or [`ParseError::NotAMapping`].
pub fn split_frontmatter(input: &str) -> Result<(serde_yaml_ng::Mapping, &str), ParseError> {
    // An editor-saved page may carry a UTF-8 BOM and/or CRLF line endings; both are ordinary files.
    // Slices (not copies) are returned, so the body stays byte-verbatim.
    let after_open = strip_bom(input)
        .strip_prefix("---\n")
        .or_else(|| strip_bom(input).strip_prefix("---\r\n"))
        .ok_or(ParseError::MissingFrontmatter)?;

    // Find the closing delimiter: a `---` line. Match either
    // `\n---\n` (delimiter followed by body) or `\n---` at end of
    // input (delimiter is the last line with no trailing newline).
    let (yaml, body) = split_at_close(after_open).ok_or(ParseError::UnterminatedFrontmatter)?;

    match serde_yaml_ng::from_str(yaml)? {
        serde_yaml_ng::Value::Mapping(m) => Ok((m, body)),
        // An empty block (`---\n---`) is an empty mapping, not "not a mapping".
        serde_yaml_ng::Value::Null => Ok((serde_yaml_ng::Mapping::new(), body)),
        _ => Err(ParseError::NotAMapping),
    }
}

/// Serialise `fields` as a frontmatter block followed by `body` — the inverse of
/// [`split_frontmatter`]. Key order is the mapping's order; comments are not representable.
///
/// # Errors
///
/// [`ParseError::Yaml`] when a value cannot be serialised.
pub fn render_page(fields: &serde_yaml_ng::Mapping, body: &str) -> Result<String, ParseError> {
    // `to_string` terminates with a newline, so the closing `---` follows on
    // its own line directly after the serialized block. An empty mapping
    // serialises as `{}`, which a reader would take for a one-line flow map:
    // emit an empty block instead.
    let yaml = if fields.is_empty() {
        String::new()
    } else {
        serde_yaml_ng::to_string(fields)?
    };
    Ok(format!("---\n{yaml}---\n{body}"))
}

/// Set a boolean frontmatter field on a page and re-serialize the whole
/// markdown document (frontmatter block + unchanged body).
///
/// Used to stamp a soft-delete marker (`archived: true`) onto a page's
/// canonical markdown so the derived index can drop the page while the
/// LaneStore retains the record and a from-scratch rebuild skips it
/// (escurel-index `delete_page`, #300).
///
/// The frontmatter is re-serialized from the parsed YAML mapping, so key
/// order and formatting are normalised and any comments in the block are not
/// preserved — acceptable for a page being retracted.
///
/// # Errors
///
/// Returns [`ParseError`] when `input` is not a well-formed page, or when the
/// mapping cannot be re-serialized as YAML.
pub fn set_frontmatter_bool(input: &str, key: &str, value: bool) -> Result<String, ParseError> {
    let page = parse(input)?;
    let mut fields = page.frontmatter.fields.clone();
    fields.insert(YamlValue::String(key.to_owned()), YamlValue::Bool(value));
    render_page(&fields, page.body)
}

/// Set (or clear) a string-valued frontmatter key, re-serialising the page.
///
/// `Some(value)` inserts or replaces; `None` removes. Same normalisation
/// caveat as [`set_frontmatter_bool`]: key order and formatting are
/// normalised and comments in the block are not preserved.
///
/// Exists so callers that own a server-stamped key — `scenario:` for a branch
/// write (#512) — do not hand-roll frontmatter serialisation in their own
/// crate. This one owns the YAML dependency; nobody else should have to.
///
/// # Errors
///
/// Returns [`ParseError`] when `input` is not a well-formed page, or when the
/// mapping cannot be re-serialized as YAML.
pub fn set_frontmatter_str(
    input: &str,
    key: &str,
    value: Option<&str>,
) -> Result<String, ParseError> {
    let page = parse(input)?;
    let mut fields = page.frontmatter.fields.clone();
    match value {
        Some(v) => {
            fields.insert(
                YamlValue::String(key.to_owned()),
                YamlValue::String(v.to_owned()),
            );
        }
        None => {
            fields.remove(YamlValue::String(key.to_owned()));
        }
    }
    render_page(&fields, page.body)
}

/// Set the top-level frontmatter key `key` to the YAML scalar `literal`
/// **in place**: only that key's line (and any indented continuation lines a
/// previous value spread over) changes; every other line — comments, key
/// order, quoting, the body — is kept byte for byte. A key that is absent is
/// appended as the block's last line.
///
/// For a server-owned scalar on a page a human also edits (`token_epoch:` on an
/// agent page), where re-serialising the whole block ([`set_frontmatter_str`])
/// would throw the author's formatting away. The result is re-parsed, so a
/// literal that is not a scalar of its own is refused rather than written.
///
/// # Errors
///
/// [`ParseError`] when `input` is not a well-formed page, or when the patched
/// page no longer parses with `key` set to `literal`.
pub fn set_frontmatter_scalar(input: &str, key: &str, literal: &str) -> Result<String, ParseError> {
    let before = parse(input)?;
    let bom = input.len() - strip_bom(input).len();
    let first = input[bom..]
        .split_inclusive('\n')
        .next()
        .ok_or(ParseError::MissingFrontmatter)?;
    let eol = if first.ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut offset = bom + first.len();
    let mut replace: Option<(usize, usize)> = None;
    let mut close = None;
    for line in input[offset..].split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "---" {
            close = Some(offset);
            break;
        }
        match &mut replace {
            // A continuation of the value being replaced: indented, or a blank line inside it.
            Some((_, end))
                if *end == offset && (content.starts_with([' ', '\t']) || content.is_empty()) =>
            {
                *end += line.len();
            }
            None if content
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':')) =>
            {
                replace = Some((offset, offset + line.len()));
            }
            _ => {}
        }
        offset += line.len();
    }
    let close = close.ok_or(ParseError::UnterminatedFrontmatter)?;
    let (start, end) = replace.unwrap_or((close, close));
    let out = format!("{}{key}: {literal}{eol}{}", &input[..start], &input[end..]);
    let after = parse(&out)?;
    let want: YamlValue = serde_yaml_ng::from_str(literal)?;
    let mut expected = before.frontmatter.fields.clone();
    expected.insert(YamlValue::String(key.to_owned()), want);
    if after.frontmatter.fields != expected || after.body != before.body {
        return Err(ParseError::UnpatchableKey(key.to_owned()));
    }
    Ok(out)
}

/// Locate the closing `---` line. Returns `(yaml_block, body_slice)`
/// where `body_slice` starts at the first character after the
/// closing delimiter's trailing newline (or is empty if the
/// delimiter is the last line).
/// `input` without a leading UTF-8 byte-order mark.
pub(crate) fn strip_bom(input: &str) -> &str {
    input.strip_prefix('\u{feff}').unwrap_or(input)
}

fn split_at_close(after_open: &str) -> Option<(&str, &str)> {
    // Walk line-starts in the remainder. A closing delimiter is a
    // line whose entire content is `---`.
    let mut cursor = 0usize;
    let bytes = after_open.as_bytes();
    while cursor < bytes.len() {
        // Position of the next newline (or end of input).
        let line_end = after_open[cursor..]
            .find('\n')
            .map_or(bytes.len(), |off| cursor + off);
        let line = after_open[cursor..line_end].trim_end_matches('\r');
        if line == "---" {
            let yaml = &after_open[..cursor.saturating_sub(1)];
            // Skip past `---` and the following `\n` if present.
            let body_start = (line_end + 1).min(bytes.len());
            return Some((yaml, &after_open[body_start..]));
        }
        // Advance past this line and its newline.
        cursor = (line_end + 1).min(bytes.len());
        if cursor == bytes.len() && line_end == bytes.len() {
            break;
        }
    }
    None
}
