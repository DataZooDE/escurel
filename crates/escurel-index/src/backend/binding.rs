//! Parsing the per-skill `backend:` frontmatter block.
//!
//! A skill page MAY declare a `backend:` block selecting which
//! [`InstanceBackend`](super::InstanceBackend) materialises and reads its
//! instances. A skill with no `backend:` block — every skill in the corpus
//! today — defaults to [`BackendKind::Markdown`], so this is fully
//! backward-compatible (REQ-BK-01).
//!
//! PR-1 recognises only `kind: markdown` (or an absent block); the
//! `sql_view` / `document` arms and their `source` / `accepts` fields land
//! with their backends. The struct is shaped so those fields are additive.

use super::BackendKind;

/// The parsed `backend:` block off a skill page's frontmatter.
///
/// In PR-1 this carries only the discriminant; later PRs add the
/// backend-specific configuration (`source`, `project`, `search_text` for
/// SQL; `accepts`, `extract`, `chunk`, `retrieval` for documents) as
/// additional fields, none of which break the markdown default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendBinding {
    pub kind: BackendKind,
}

impl Default for BackendBinding {
    fn default() -> Self {
        Self {
            kind: BackendKind::Markdown,
        }
    }
}

impl BackendBinding {
    /// Parse the `backend:` block from a skill page's frontmatter JSON.
    ///
    /// An absent block, or a `kind:` that is absent or unrecognised,
    /// yields the markdown default — preserving today's behaviour where
    /// every skill is markdown-backed. PR-2 tightens an *unknown* kind
    /// into a hard error once `sql_view` / `document` are real arms; until
    /// then there is no fixture carrying a `backend:` block, so the lenient
    /// fallback is a no-op on the current corpus.
    #[must_use]
    pub fn parse(fm: &serde_json::Value) -> Self {
        let Some(block) = fm.get("backend").and_then(serde_json::Value::as_object) else {
            return Self::default();
        };
        match block.get("kind").and_then(serde_json::Value::as_str) {
            Some("markdown") | None => Self {
                kind: BackendKind::Markdown,
            },
            // Unknown kinds fall back to markdown for now (see doc above).
            Some(_) => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_backend_binding_absent_block_is_markdown() {
        assert_eq!(
            BackendBinding::parse(&json!({"type": "skill", "id": "customer"})),
            BackendBinding {
                kind: BackendKind::Markdown
            }
        );
    }

    #[test]
    fn parse_backend_binding_explicit_markdown() {
        assert_eq!(
            BackendBinding::parse(&json!({"backend": {"kind": "markdown"}})),
            BackendBinding {
                kind: BackendKind::Markdown
            }
        );
    }

    #[test]
    fn parse_backend_binding_unknown_kind_falls_back_to_markdown() {
        // PR-1 is lenient; PR-2 turns this into an error once sql_view is real.
        assert_eq!(
            BackendBinding::parse(&json!({"backend": {"kind": "sql_view"}})),
            BackendBinding {
                kind: BackendKind::Markdown
            }
        );
    }
}
