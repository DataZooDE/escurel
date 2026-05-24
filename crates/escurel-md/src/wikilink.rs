//! Typed wikilink parser.
//!
//! Escurel pages cite each other through wikilinks of the form
//!
//! ```text
//! [[skill::id#anchor@version|alias]]
//! ```
//!
//! All segments except the link target are optional. A bare
//! `[[id]]` is interpreted as an id-only reference; the resolver
//! (in `escurel-index`) decides at lookup time whether that id
//! names a skill or an instance.
//!
//! ## Why regex and not a markdown AST
//!
//! The Escurel storage spec is explicit about this:
//!
//! > Parse wikilinks using the regex-plus-code-region-stripping
//! > parser (do **not** use a markdown AST library — they
//! > fragment text on `[`).
//!
//! Markdown ASTs treat `[` as the start of a link and split runs
//! of text whenever they see one, which destroys the
//! `[[skill::id]]` token before we ever see it. We strip fenced
//! and inline code regions ourselves and run a regex over what
//! remains.

use std::sync::LazyLock;

use regex::Regex;

/// One wikilink occurrence, decomposed.
///
/// Shape mirrors `WikilinkParsed` in `docs/spec/protocol.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikilinkParsed {
    /// `skill` segment of a typed `[[skill::id]]`. `None` for a
    /// bare `[[id]]`.
    pub skill: Option<String>,
    /// `id` segment. `None` only when the input was malformed (a
    /// typed link like `[[skill::]]` with nothing after `::`).
    pub id: Option<String>,
    /// `#anchor` segment, if present.
    pub anchor: Option<String>,
    /// `@version` segment, if present.
    pub version: Option<String>,
    /// `|alias` (display text) segment, if present.
    pub alias: Option<String>,
}

/// Parse every wikilink in `markdown`, skipping any wikilink that
/// sits inside a fenced code block (` ``` `) or an inline code
/// span (`` `…` ``).
///
/// Returns links in document order.
pub fn parse_wikilinks(markdown: &str) -> Vec<WikilinkParsed> {
    let stripped = strip_code_regions(markdown);
    WIKILINK_RE
        .captures_iter(&stripped)
        .filter_map(|cap| parse_one(&cap[1]))
        .collect()
}

/// Parse the contents between `[[` and `]]` into a [`WikilinkParsed`].
/// Returns `None` if the content can't yield even an id or a skill
/// (e.g. only whitespace).
fn parse_one(content: &str) -> Option<WikilinkParsed> {
    let (target, alias) = split_first(content, '|');
    let (target, version) = split_first(target, '@');
    let (target, anchor) = split_first(target, '#');
    let (skill, id) = match target.split_once("::") {
        Some((s, i)) => (some_if_nonempty(s), some_if_nonempty(i)),
        None => (None, some_if_nonempty(target)),
    };

    if skill.is_none() && id.is_none() {
        return None;
    }

    Some(WikilinkParsed {
        skill,
        id,
        anchor: anchor.and_then(some_if_nonempty),
        version: version.and_then(some_if_nonempty),
        alias: alias.and_then(some_if_nonempty),
    })
}

/// Split on the first occurrence of `sep`. Trims surrounding
/// whitespace on both halves.
fn split_first(s: &str, sep: char) -> (&str, Option<&str>) {
    match s.split_once(sep) {
        Some((l, r)) => (l.trim(), Some(r.trim())),
        None => (s.trim(), None),
    }
}

fn some_if_nonempty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_owned())
    }
}

/// Replace fenced code blocks and inline code spans with
/// equal-length whitespace, preserving newlines and byte offsets
/// so the wikilink regex never sees them.
fn strip_code_regions(input: &str) -> String {
    // Pass 1: fenced code blocks. A line whose trimmed-leading
    // content begins with ``` toggles in/out of a fence and is
    // itself blanked (so a ``` line never matches a wikilink).
    let mut out = String::with_capacity(input.len());
    let mut in_fence = false;
    for line in input.split_inclusive('\n') {
        let is_fence_marker = line.trim_start().starts_with("```");
        if is_fence_marker || in_fence {
            push_blanked(&mut out, line);
            if is_fence_marker {
                in_fence = !in_fence;
            }
        } else {
            out.push_str(line);
        }
    }

    // Pass 2: inline code spans on what's left.
    INLINE_CODE_RE
        .replace_all(&out, |caps: &regex::Captures| " ".repeat(caps[0].len()))
        .into_owned()
}

fn push_blanked(out: &mut String, line: &str) {
    for ch in line.chars() {
        out.push(if ch == '\n' { '\n' } else { ' ' });
    }
}

static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\[\]\r\n]+?)\]\]").expect("wikilink regex compiles"));

static INLINE_CODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`[^`\n]*`").expect("inline code regex compiles"));
