//! The one-way migration of the page-kind key: `type: skill|instance` -> `kind: skill|instance`.
//!
//! This is a TEXT edit of the leading frontmatter block, not a YAML re-serialisation:
//! [`set_frontmatter_bool`](crate::set_frontmatter_bool) normalises key order and drops comments,
//! which is not acceptable for rewriting every page a tenant has. Only the single top-level
//! `type:` line changes; every other byte of the page is preserved, and a trailing comment on
//! that line is kept.
//!
//! A user's instance may legitimately carry its OWN data field named `type:` (`type: invoice`).
//! Only a top-level `type:` whose value is exactly `skill` or `instance` is the page kind, so a
//! page that has some other `type:` value is left alone and reported.

/// What [`rewrite_legacy_type_key`] did (or why it did nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindRewrite {
    /// The page used the legacy key; this is the page with `kind:` in its place.
    Rewritten(String),
    /// The page already uses `kind:` and has no legacy `type:` page-kind line. Nothing to do.
    AlreadyKind,
    /// The page has BOTH a top-level `kind:` and a legacy page-kind `type:`. Never auto-fixed.
    Conflict,
    /// The page has neither (not a well-formed page today) or its top-level `type:` carries some
    /// other value: a user data field, not the page kind. Left untouched.
    NotAPageKind,
}

/// Rewrite the legacy page-kind key of one page. See the module docs for the exact rule.
#[must_use]
pub fn rewrite_legacy_type_key(input: &str) -> KindRewrite {
    // The block is `---\n` .. a line that is exactly `---` (CRLF tolerated). Anything else is not a
    // page this function may edit.
    let Some(first) = input.split_inclusive('\n').next() else {
        return KindRewrite::NotAPageKind;
    };
    if first != "---\n" {
        return KindRewrite::NotAPageKind;
    }
    let mut offset = first.len();
    let mut legacy_line: Option<(usize, usize)> = None; // byte range of the `type` key text
    let mut has_kind = false;
    let mut closed = false;
    for line in input[offset..].split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "---" {
            closed = true;
            break;
        }
        // Top level only: the key starts in column 0 (no indentation, no list dash).
        if let Some(rest) = content.strip_prefix("type:") {
            if page_kind_value(rest) {
                legacy_line = Some((offset, offset + "type".len()));
            }
        } else if content.starts_with("kind:") {
            has_kind = true;
        }
        offset += line.len();
    }
    if !closed {
        return KindRewrite::NotAPageKind;
    }
    match (legacy_line, has_kind) {
        (Some(_), true) => KindRewrite::Conflict,
        (Some((start, end)), false) => {
            let mut out = String::with_capacity(input.len());
            out.push_str(&input[..start]);
            out.push_str("kind");
            out.push_str(&input[end..]);
            KindRewrite::Rewritten(out)
        }
        (None, true) => KindRewrite::AlreadyKind,
        (None, false) => KindRewrite::NotAPageKind,
    }
}

/// `true` when the text after `type:` is exactly `skill` or `instance` (bare or quoted), allowing a
/// trailing `# comment`. Any other value is a user data field, not the page kind.
fn page_kind_value(rest: &str) -> bool {
    let value = rest.split('#').next().unwrap_or("").trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    value == "skill" || value == "instance"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(input: &str) -> String {
        match rewrite_legacy_type_key(input) {
            KindRewrite::Rewritten(s) => s,
            other => panic!("expected Rewritten, got {other:?}"),
        }
    }

    #[test]
    fn rewrites_a_bare_skill_key_and_nothing_else() {
        let input = "---\ntype: skill\nid: customer\ndescription: A buyer.\n---\n\n# Customer\n\ntype: skill appears in prose too.\n";
        let out = rewritten(input);
        assert_eq!(
            out,
            "---\nkind: skill\nid: customer\ndescription: A buyer.\n---\n\n# Customer\n\ntype: skill appears in prose too.\n"
        );
    }

    #[test]
    fn rewrites_instance_quoted_and_with_odd_spacing_and_a_comment() {
        assert_eq!(
            rewritten("---\ntype: \"instance\"\nid: a\n---\nx\n"),
            "---\nkind: \"instance\"\nid: a\n---\nx\n"
        );
        assert_eq!(
            rewritten("---\nid: a\ntype:   'instance'   # the page kind\n---\n"),
            "---\nid: a\nkind:   'instance'   # the page kind\n---\n"
        );
    }

    #[test]
    fn keeps_crlf_line_endings_on_the_edited_line() {
        let out = rewritten("---\ntype: skill\r\nid: a\r\n---\r\nbody\r\n");
        assert_eq!(out, "---\nkind: skill\r\nid: a\r\n---\r\nbody\r\n");
    }

    #[test]
    fn is_idempotent_a_migrated_page_is_already_kind() {
        let migrated = rewritten("---\ntype: instance\nid: a\n---\nb\n");
        assert_eq!(rewrite_legacy_type_key(&migrated), KindRewrite::AlreadyKind);
    }

    #[test]
    fn both_keys_is_a_conflict_never_auto_fixed() {
        assert_eq!(
            rewrite_legacy_type_key("---\ntype: skill\nkind: skill\nid: a\n---\n"),
            KindRewrite::Conflict
        );
    }

    #[test]
    fn a_user_data_field_named_type_is_not_the_page_kind() {
        // `type: invoice` is the instance's own data; the page kind is missing, which is invalid
        // today and not something this tool may invent.
        assert_eq!(
            rewrite_legacy_type_key("---\nid: a\nskill: doc\ntype: invoice\n---\nb\n"),
            KindRewrite::NotAPageKind
        );
        // The page kind is already `kind:`; the user's `type: invoice` is theirs and stays.
        assert_eq!(
            rewrite_legacy_type_key("---\nkind: instance\nskill: doc\ntype: invoice\n---\nb\n"),
            KindRewrite::AlreadyKind
        );
    }

    #[test]
    fn a_nested_or_indented_type_line_is_never_touched() {
        let nested = "---\nkind: skill\nbackend:\n  type: skill\nid: a\n---\n";
        assert_eq!(rewrite_legacy_type_key(nested), KindRewrite::AlreadyKind);
        let only_nested = "---\nid: a\nbackend:\n  type: instance\n---\n";
        assert_eq!(
            rewrite_legacy_type_key(only_nested),
            KindRewrite::NotAPageKind
        );
    }

    #[test]
    fn only_the_frontmatter_block_is_considered() {
        // A page with no frontmatter, or a `type: skill` line only in the body.
        assert_eq!(
            rewrite_legacy_type_key("# no frontmatter\ntype: skill\n"),
            KindRewrite::NotAPageKind
        );
        assert_eq!(
            rewrite_legacy_type_key("---\nid: a\n---\ntype: skill\n"),
            KindRewrite::NotAPageKind
        );
    }

    #[test]
    fn an_unterminated_frontmatter_block_is_not_touched() {
        assert_eq!(
            rewrite_legacy_type_key("---\ntype: skill\nid: a\n"),
            KindRewrite::NotAPageKind
        );
    }
}
