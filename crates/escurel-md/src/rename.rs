//! REMOVE after v2027.xx: legacy migration shim (see docs/notes/legacy-migration-shims.md).
//!
//! `migrate-kind --rename-field OLD=NEW`: a tenant's own DOMAIN field that collides with the
//! page-kind key (`kind: bus` on a `system` instance, declared in its skill's
//! `optional_frontmatter: [kind, ...]`) is renamed inside the same migration that turns the legacy
//! `type:` into `kind:`, instead of making an operator hand-edit live pages first.
//!
//! Like [`rewrite_legacy_type_key`](crate::rewrite_legacy_type_key) this is a TEXT edit: only the
//! top-level key text (and the elements of the skill's declared-field lists) change, every other
//! byte of the page is kept, and a value, a trailing comment and the line ending stay as they were.
//!
//! * An INSTANCE page is only touched when it still carries the legacy page-kind `type:` (it is
//!   being migrated in the same write); its `OLD` key is renamed to `NEW`, the value preserved.
//!   The one exception: `OLD = kind` whose value is `skill`/`instance` is the page kind itself, so
//!   the page stays a real conflict.
//! * A SKILL page's `optional_frontmatter` / `required_frontmatter` entries `OLD` become `NEW`
//!   (flow `[a, b]` and block `- a` lists), whether the page still says `type: skill` or already
//!   says `kind: skill` (so a second run, or a half-migrated tenant, converges).
//! * `NEW` already present (as a key of the page, or in the same declared list) is a
//!   [`PageRewriteOutcome::Collision`]: never overwritten, reported.
//! * References to `OLD` elsewhere in a skill (SQL views, listing columns, query docs) are NOT
//!   rewritten; [`field_references`] finds them so the report can warn the owner.

use crate::legacy::{KindRewrite, rewrite_legacy_type_key};

/// One requested top-level frontmatter key rename, `OLD=NEW`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRename {
    pub from: String,
    pub to: String,
}

impl FieldRename {
    /// Parse the `OLD=NEW` spelling used by the CLI flag and the job's env var.
    ///
    /// # Errors
    /// A message when either side is not a plain frontmatter key, or both sides are the same.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let Some((from, to)) = spec.split_once('=') else {
            return Err(format!("`{spec}`: expected OLD=NEW"));
        };
        let (from, to) = (from.trim(), to.trim());
        for side in [from, to] {
            let ok = !side.is_empty()
                && side
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && !side.starts_with(|c: char| c.is_ascii_digit() || c == '-');
            if !ok {
                return Err(format!(
                    "`{spec}`: `{side}` is not a plain frontmatter key (letters, digits, `_`, `-`)"
                ));
            }
        }
        if from == to {
            return Err(format!("`{spec}`: both sides are the same key"));
        }
        Ok(Self {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }
}

/// What [`rewrite_page_kind`] did to one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageRewriteOutcome {
    /// The page was processed (see [`PageRewrite::changed`] for whether anything changed).
    Done(PageRewrite),
    /// BOTH keys (`kind:` and a legacy page-kind `type:`) and no rename applies: never auto-fixed.
    Conflict,
    /// The rename target already exists on the page (or in the same declared list).
    Collision(FieldRename),
    /// Neither a page kind nor a legacy one: left untouched.
    NotAPageKind,
}

/// A processed page: the new text and what changed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRewrite {
    pub content: String,
    /// The legacy `type:` was rewritten to `kind:`.
    pub kind_changed: bool,
    /// Domain fields of an instance page that were renamed.
    pub instance_fields: Vec<FieldRename>,
    /// Entries of a skill's declared-field lists that were renamed.
    pub declarations: Vec<FieldRename>,
}

impl PageRewrite {
    /// `true` when the page's bytes differ from the input.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.kind_changed || !self.instance_fields.is_empty() || !self.declarations.is_empty()
    }
}

/// Migrate one page: rename the requested domain fields, then the legacy page-kind key, then a
/// skill's declared-field lists. With no `renames` this is exactly
/// [`rewrite_legacy_type_key`](crate::rewrite_legacy_type_key).
#[must_use]
pub fn rewrite_page_kind(input: &str, renames: &[FieldRename]) -> PageRewriteOutcome {
    // 1. A page that is being migrated (it still has the legacy page-kind `type:`) gets its colliding
    //    domain fields renamed first, so the page-kind rewrite below no longer sees a second `kind:`.
    let mut current = input.to_owned();
    let mut instance_fields = Vec::new();
    if !renames.is_empty() && has_legacy_page_kind(&current) {
        for r in renames {
            match rename_instance_field(&current, r) {
                Edit::None => {}
                Edit::Collision => return PageRewriteOutcome::Collision(r.clone()),
                Edit::Renamed(next) => {
                    current = next;
                    instance_fields.push(r.clone());
                }
            }
        }
    }
    // 2. The page kind itself.
    let kind_changed = match rewrite_legacy_type_key(&current) {
        KindRewrite::Rewritten(next) => {
            current = next;
            true
        }
        KindRewrite::AlreadyKind => false,
        KindRewrite::Conflict => return PageRewriteOutcome::Conflict,
        KindRewrite::NotAPageKind => return PageRewriteOutcome::NotAPageKind,
    };
    // 3. A skill's declared-field lists (also on a skill that already says `kind: skill`).
    let mut declarations = Vec::new();
    if !renames.is_empty() && is_skill_page(&current) {
        for r in renames {
            match rename_declared_field(&current, r) {
                Edit::None => {}
                Edit::Collision => return PageRewriteOutcome::Collision(r.clone()),
                Edit::Renamed(next) => {
                    current = next;
                    declarations.push(r.clone());
                }
            }
        }
    }
    PageRewriteOutcome::Done(PageRewrite {
        content: current,
        kind_changed,
        instance_fields,
        declarations,
    })
}

/// Where `key` is mentioned in a skill page OUTSIDE the declared-field lists and the page-kind
/// line: other frontmatter lines (a SQL view, listing columns) and code in the body (fenced blocks
/// and inline `code`). Each hit is `(1-based line number, trimmed line)`. These are the places a
/// rename does not rewrite, for the operator to review.
#[must_use]
pub fn field_references(input: &str, key: &str) -> Vec<(usize, String)> {
    let lines = split_lines(input);
    let Some(range) = frontmatter_range(input, &lines) else {
        return Vec::new();
    };
    let declared = declared_line_indexes(&lines, range);
    let mut hits = Vec::new();
    let mut in_fence = false;
    for (i, l) in lines.iter().enumerate() {
        let text = l.text;
        let hit = if i > range.0 && i < range.1 {
            let is_kind_line = top_key(text)
                .is_some_and(|(k, rest)| (k == "kind" || k == "type") && page_kind_value(rest));
            !declared.contains(&i) && !is_kind_line && contains_word(text, key)
        } else if i > range.1 {
            let t = text.trim_start();
            if t.starts_with("```") || t.starts_with("~~~") {
                in_fence = !in_fence;
                false
            } else if in_fence {
                contains_word(text, key)
            } else {
                inline_code(text).any(|c| contains_word(c, key))
            }
        } else {
            false
        };
        if hit {
            hits.push((i + 1, text.trim().chars().take(160).collect()));
        }
    }
    hits
}

// ---------------------------------------------------------------------------------------------
// text plumbing

struct Line<'a> {
    /// Byte offset of the line start in the input (a BOM belongs to line 0's text).
    start: usize,
    /// The line without its `\n` / `\r\n`.
    text: &'a str,
}

fn split_lines(input: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for raw in input.split_inclusive('\n') {
        out.push(Line {
            start,
            text: raw.trim_end_matches(['\r', '\n']),
        });
        start += raw.len();
    }
    out
}

/// `(open, close)` line indexes of the leading `---` block, or `None` for anything else.
fn frontmatter_range(input: &str, lines: &[Line<'_>]) -> Option<(usize, usize)> {
    let first = lines.first()?;
    let bom = input.len() - crate::strip_bom(input).len();
    if first.text[bom.min(first.text.len())..] != *"---" {
        return None;
    }
    let close = lines.iter().skip(1).position(|l| l.text == "---")? + 1;
    Some((0, close))
}

/// A top-level `key: rest` line: column 0, a plain key. `(key, text after the colon)`.
fn top_key(text: &str) -> Option<(&str, &str)> {
    if text.starts_with([' ', '\t', '-', '#']) {
        return None;
    }
    let (k, rest) = text.split_once(':')?;
    let plain = !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    plain.then_some((k, rest))
}

/// The value of a scalar `rest`: no trailing `# comment`, no surrounding quotes.
fn scalar(rest: &str) -> &str {
    let v = rest.split('#').next().unwrap_or("").trim();
    v.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(v)
}

fn page_kind_value(rest: &str) -> bool {
    matches!(scalar(rest), "skill" | "instance")
}

fn has_legacy_page_kind(input: &str) -> bool {
    let lines = split_lines(input);
    let Some((open, close)) = frontmatter_range(input, &lines) else {
        return false;
    };
    lines[open + 1..close]
        .iter()
        .any(|l| top_key(l.text).is_some_and(|(k, rest)| k == "type" && page_kind_value(rest)))
}

fn is_skill_page(input: &str) -> bool {
    let lines = split_lines(input);
    let Some((open, close)) = frontmatter_range(input, &lines) else {
        return false;
    };
    lines[open + 1..close]
        .iter()
        .any(|l| top_key(l.text).is_some_and(|(k, rest)| k == "kind" && scalar(rest) == "skill"))
}

enum Edit {
    None,
    Collision,
    Renamed(String),
}

/// Rename the top-level key `r.from` of an instance page (value, comment and EOL untouched).
fn rename_instance_field(input: &str, r: &FieldRename) -> Edit {
    let lines = split_lines(input);
    let Some((open, close)) = frontmatter_range(input, &lines) else {
        return Edit::None;
    };
    let body = &lines[open + 1..close];
    let Some(at) = body
        .iter()
        .find(|l| top_key(l.text).is_some_and(|(k, _)| k == r.from))
    else {
        return Edit::None;
    };
    // `kind: instance|skill` is the page kind itself, not a domain field: a real conflict.
    if r.from == "kind" && top_key(at.text).is_some_and(|(_, rest)| page_kind_value(rest)) {
        return Edit::None;
    }
    if body
        .iter()
        .any(|l| top_key(l.text).is_some_and(|(k, _)| k == r.to))
    {
        return Edit::Collision;
    }
    let mut out = String::with_capacity(input.len() + r.to.len());
    out.push_str(&input[..at.start]);
    out.push_str(&r.to);
    out.push_str(&input[at.start + r.from.len()..]);
    Edit::Renamed(out)
}

const DECLARED_LISTS: [&str; 2] = ["optional_frontmatter", "required_frontmatter"];

/// Byte span `(start, end)` of the bare name inside one list element (`  - 'kind'   # note`,
/// `"kind"`, `kind `): past leading blanks/dash/quote, up to the closing quote or blank.
fn element_span(s: &str) -> Option<(usize, usize)> {
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'-') {
        i += 1;
    }
    let quote = matches!(b.get(i), Some(b'"' | b'\''));
    let q = if quote { Some(b[i]) } else { None };
    if quote {
        i += 1;
    }
    let start = i;
    while i < b.len() {
        let c = b[i];
        let stop = match q {
            Some(q) => c == q,
            None => matches!(c, b' ' | b'\t' | b',' | b']' | b'#'),
        };
        if stop {
            break;
        }
        i += 1;
    }
    (i > start).then_some((start, i))
}

/// Indexes of the lines that make up the declared-field lists (key lines and their block elements).
fn declared_line_indexes(lines: &[Line<'_>], range: (usize, usize)) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = range.0 + 1;
    while i < range.1 {
        if let Some((k, rest)) = top_key(lines[i].text)
            && DECLARED_LISTS.contains(&k)
        {
            out.push(i);
            if rest.trim().is_empty() || rest.trim_start().starts_with('#') {
                let mut j = i + 1;
                while j < range.1 {
                    let t = lines[j].text;
                    if t.trim().is_empty() {
                        j += 1;
                        continue;
                    }
                    if t.starts_with([' ', '\t']) && t.trim_start().starts_with('-') {
                        out.push(j);
                        j += 1;
                    } else {
                        break;
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Rename `r.from` inside a skill's declared-field lists.
fn rename_declared_field(input: &str, r: &FieldRename) -> Edit {
    let lines = split_lines(input);
    let Some(range) = frontmatter_range(input, &lines) else {
        return Edit::None;
    };
    // Collect each list as (name span in the input, name) groups.
    let mut lists: Vec<Vec<(usize, usize, String)>> = Vec::new();
    let mut i = range.0 + 1;
    while i < range.1 {
        let Some((k, rest)) = top_key(lines[i].text) else {
            i += 1;
            continue;
        };
        if !DECLARED_LISTS.contains(&k) {
            i += 1;
            continue;
        }
        let mut items = Vec::new();
        let trimmed = rest.trim_start();
        if let Some(inner) = trimmed.strip_prefix('[') {
            // Flow list on one line: split the inside of `[...]` at commas.
            let base = lines[i].start + (lines[i].text.len() - inner.len());
            let end = inner.find(']').unwrap_or(inner.len());
            let mut off = 0;
            for part in inner[..end].split(',') {
                if let Some((s, e)) = element_span(part) {
                    items.push((base + off + s, base + off + e, part[s..e].to_owned()));
                }
                off += part.len() + 1;
            }
            i += 1;
        } else {
            i += 1;
            while i < range.1 {
                let t = lines[i].text;
                if t.trim().is_empty() {
                    i += 1;
                    continue;
                }
                if t.starts_with([' ', '\t']) && t.trim_start().starts_with('-') {
                    if let Some((s, e)) = element_span(t) {
                        items.push((lines[i].start + s, lines[i].start + e, t[s..e].to_owned()));
                    }
                    i += 1;
                } else {
                    break;
                }
            }
        }
        lists.push(items);
    }
    let mut edits: Vec<(usize, usize)> = Vec::new();
    for items in &lists {
        let has_from = items.iter().position(|(_, _, n)| *n == r.from);
        if let Some(at) = has_from {
            if items.iter().any(|(_, _, n)| *n == r.to) {
                return Edit::Collision;
            }
            edits.push((items[at].0, items[at].1));
        }
    }
    if edits.is_empty() {
        return Edit::None;
    }
    edits.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let mut out = input.to_owned();
    for (s, e) in edits {
        out.replace_range(s..e, &r.to);
    }
    Edit::Renamed(out)
}

/// `true` when `key` occurs in `text` as a whole identifier.
fn contains_word(text: &str, key: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(key).any(|(i, m)| {
        let before = text[..i].chars().next_back();
        let after = text[i + m.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// The contents of the inline `code` spans of one body line.
fn inline_code(text: &str) -> impl Iterator<Item = &str> {
    text.split('`').skip(1).step_by(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_rename() -> Vec<FieldRename> {
        vec![FieldRename::parse("kind=system_kind").unwrap()]
    }

    fn done(o: PageRewriteOutcome) -> PageRewrite {
        match o {
            PageRewriteOutcome::Done(p) => p,
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn parse_accepts_old_eq_new_and_refuses_junk() {
        assert_eq!(
            FieldRename::parse(" kind = system_kind ").unwrap(),
            FieldRename {
                from: "kind".into(),
                to: "system_kind".into()
            }
        );
        for bad in ["kind", "=x", "kind=", "kind=kind", "a b=c", "a=b:c", "1a=b"] {
            assert!(FieldRename::parse(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn heron_instance_with_type_and_a_kind_data_field_is_renamed_then_migrated() {
        let input = "---\ntype: instance\nskill: system\nid: lexware\nkind: saas-api   # transport\nendpoint: https://x\n---\n# lexware\n\nkind: saas-api in prose stays.\n";
        let p = done(rewrite_page_kind(input, &kind_rename()));
        assert_eq!(
            p.content,
            "---\nkind: instance\nskill: system\nid: lexware\nsystem_kind: saas-api   # transport\nendpoint: https://x\n---\n# lexware\n\nkind: saas-api in prose stays.\n"
        );
        assert!(p.kind_changed);
        assert_eq!(p.instance_fields, kind_rename());
        assert!(p.declarations.is_empty());
    }

    #[test]
    fn without_the_rename_the_same_page_is_still_a_conflict() {
        let input = "---\ntype: instance\nskill: system\nid: lexware\nkind: saas-api\n---\n";
        assert_eq!(rewrite_page_kind(input, &[]), PageRewriteOutcome::Conflict);
    }

    #[test]
    fn a_real_page_kind_in_the_old_key_is_still_a_conflict_with_the_rename() {
        // `kind: instance` beside `type: instance` is the page kind twice, not a domain field.
        let input = "---\ntype: instance\nkind: instance\nskill: doc\nid: both\n---\n";
        assert_eq!(
            rewrite_page_kind(input, &kind_rename()),
            PageRewriteOutcome::Conflict
        );
    }

    #[test]
    fn an_existing_target_key_is_a_collision_and_nothing_is_overwritten() {
        let input =
            "---\ntype: instance\nskill: system\nid: x\nkind: bus\nsystem_kind: other\n---\n";
        assert_eq!(
            rewrite_page_kind(input, &kind_rename()),
            PageRewriteOutcome::Collision(kind_rename().remove(0))
        );
    }

    #[test]
    fn an_already_migrated_instance_is_left_alone_and_a_second_run_is_a_no_op() {
        let first = done(rewrite_page_kind(
            "---\ntype: instance\nskill: system\nid: x\nkind: bus\n---\n",
            &kind_rename(),
        ));
        let again = done(rewrite_page_kind(&first.content, &kind_rename()));
        assert!(!again.changed());
        assert_eq!(again.content, first.content);
    }

    #[test]
    fn a_kind_only_page_without_the_legacy_type_is_not_renamed() {
        // `kind: instance` here IS the page kind: the rename must never touch it.
        let input = "---\nkind: instance\nskill: doc\nid: a\n---\n";
        let p = done(rewrite_page_kind(input, &kind_rename()));
        assert!(!p.changed());
    }

    #[test]
    fn the_system_skills_declared_fields_are_renamed_in_flow_and_block_lists() {
        let flow = "---\ntype: skill\nid: system\noptional_frontmatter: [kind, endpoint, \"owner\"]\nrequired_frontmatter: [id]\n---\n# system\n";
        let p = done(rewrite_page_kind(flow, &kind_rename()));
        assert_eq!(
            p.content,
            "---\nkind: skill\nid: system\noptional_frontmatter: [system_kind, endpoint, \"owner\"]\nrequired_frontmatter: [id]\n---\n# system\n"
        );
        assert!(p.kind_changed);
        assert_eq!(p.declarations, kind_rename());

        let block = "---\nkind: skill\nid: system\nrequired_frontmatter:\n  - id\n  - 'kind'   # transport\noptional_frontmatter:\n  - endpoint\n---\n";
        let p = done(rewrite_page_kind(block, &kind_rename()));
        assert_eq!(
            p.content,
            "---\nkind: skill\nid: system\nrequired_frontmatter:\n  - id\n  - 'system_kind'   # transport\noptional_frontmatter:\n  - endpoint\n---\n"
        );
        assert!(!p.kind_changed, "already on kind:");
        assert_eq!(p.declarations, kind_rename());
        // Converged: nothing declared under the old name any more.
        assert!(!done(rewrite_page_kind(&p.content, &kind_rename())).changed());
    }

    #[test]
    fn a_target_already_declared_in_the_same_list_is_a_collision() {
        let input = "---\nkind: skill\nid: s\noptional_frontmatter: [kind, system_kind]\n---\n";
        assert!(matches!(
            rewrite_page_kind(input, &kind_rename()),
            PageRewriteOutcome::Collision(_)
        ));
    }

    #[test]
    fn crlf_and_bom_are_preserved() {
        let input = "\u{feff}---\r\ntype: instance\r\nskill: system\r\nkind: bus\r\n---\r\nb\r\n";
        let p = done(rewrite_page_kind(input, &kind_rename()));
        assert_eq!(
            p.content,
            "\u{feff}---\r\nkind: instance\r\nskill: system\r\nsystem_kind: bus\r\n---\r\nb\r\n"
        );
    }

    #[test]
    fn nested_and_indented_keys_are_never_renamed() {
        let input = "---\ntype: instance\nskill: system\nmeta:\n  kind: nested\nkind: bus\n---\n";
        let p = done(rewrite_page_kind(input, &kind_rename()));
        assert!(p.content.contains("  kind: nested\n"));
        assert!(p.content.contains("\nsystem_kind: bus\n"));
    }

    #[test]
    fn references_outside_the_declarations_are_found_for_review() {
        let skill = "---\nkind: skill\nid: system\noptional_frontmatter: [system_kind]\nlisting_sql: select id, kind from systems\n---\n# system\n\nThe kind of system is free text.\n\n```sql\nselect kind from systems;\n```\n\nUse `kind` carefully.\n";
        let hits = field_references(skill, "kind");
        let lines: Vec<usize> = hits.iter().map(|(n, _)| *n).collect();
        assert_eq!(lines, vec![5, 12, 15], "{hits:?}");
        assert!(hits[0].1.contains("listing_sql"));
        // The page-kind line itself and prose words are not references.
        assert!(!lines.contains(&2) && !lines.contains(&9));
    }
}
