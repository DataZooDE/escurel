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
//! type: skill
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
//! The required field is `type:`, which must be `skill` or
//! `instance`. Everything else is preserved verbatim in
//! [`Frontmatter::fields`] for the indexer to project as needed.

pub mod wikilink;

use thiserror::Error;

/// The two kinds of pages an Escurel tenant carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageType {
    /// A type declaration. Defines what its instances look like.
    Skill,
    /// A memory of some skill type.
    Instance,
}

/// Parsed frontmatter for one page.
#[derive(Debug, Clone)]
pub struct Frontmatter {
    /// Convenience projection of the `type:` field.
    pub page_type: PageType,
    /// Raw frontmatter mapping (includes `type` and every other key).
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
    /// `type:` was missing or not `skill` / `instance`.
    #[error("frontmatter missing or invalid 'type' (expected 'skill' or 'instance')")]
    InvalidType,
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
/// block, the YAML is malformed, or the required `type:` field is
/// absent or unrecognised.
pub fn parse(input: &str) -> Result<Page<'_>, ParseError> {
    let after_open = input
        .strip_prefix("---\n")
        .ok_or(ParseError::MissingFrontmatter)?;

    // Find the closing delimiter: a `---` line. Match either
    // `\n---\n` (delimiter followed by body) or `\n---` at end of
    // input (delimiter is the last line with no trailing newline).
    let (yaml, body) = split_at_close(after_open).ok_or(ParseError::UnterminatedFrontmatter)?;

    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml)?;
    let mapping = match value {
        serde_yaml_ng::Value::Mapping(m) => m,
        _ => return Err(ParseError::NotAMapping),
    };

    let page_type = mapping
        .get("type")
        .and_then(serde_yaml_ng::Value::as_str)
        .and_then(|s| match s {
            "skill" => Some(PageType::Skill),
            "instance" => Some(PageType::Instance),
            _ => None,
        })
        .ok_or(ParseError::InvalidType)?;

    Ok(Page {
        frontmatter: Frontmatter {
            page_type,
            fields: mapping,
        },
        body,
    })
}

/// Locate the closing `---` line. Returns `(yaml_block, body_slice)`
/// where `body_slice` starts at the first character after the
/// closing delimiter's trailing newline (or is empty if the
/// delimiter is the last line).
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
        let line = &after_open[cursor..line_end];
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
