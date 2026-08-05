//! How an external backend presents itself on the wire.
//!
//! ## Why this is not `InstanceBackend`
//!
//! R3 of `docs/notes/complexity-reduction-plan.md` proposed routing the four
//! external backends through `escurel_index::backend::InstanceBackend`, which
//! today has exactly one implementation while `sql_view`, `document`,
//! `openapi` and `mcp` were special-cased with `backend_ref.kind` probes
//! scattered through the read tools.
//!
//! That diagnosis was half right. The duplication was real, but
//! `InstanceBackend` is the wrong home for it: its `expand` returns an
//! `ExpandedPage`, a domain value with nowhere to carry `backend_projection`,
//! `chunks_total` or a bounded block list. Those are *presentation* concerns —
//! they shape the JSON an agent receives, not what the store holds. Pushing
//! them into `escurel-index` would have moved wire-shaping into the storage
//! crate to satisfy a trait, which is a worse arrangement than the one it
//! replaced.
//!
//! So the abstraction lives here, on the presentation side, and does the job
//! the plan wanted: **one place that knows the set of backend kinds, and one
//! dispatch point per read tool.** Adding a backend means adding a variant and
//! its arm, not another `if kind == "..."` in the middle of a handler.

use serde_json::Value;

/// The backend behind an instance, as the wire layer sees it.
///
/// Read from the overlay page's `backend_ref.kind`. `Native` is the absence of
/// a `backend_ref`: an ordinary markdown page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackendView {
    Native,
    SqlView,
    Document,
    /// `openapi` and `mcp` differ in transport but are identical to the read
    /// path: both are fetched live and neither is materialised, so they share
    /// a variant rather than duplicating every arm.
    RemoteProxy,
}

impl BackendView {
    /// Classify an expanded page by its `backend_ref.kind`.
    ///
    /// An unrecognised kind is `Native`, which degrades to "show the overlay
    /// and add nothing" rather than erroring — the overlay page is always a
    /// real page, so a backend this build does not know about still reads.
    pub(super) fn of(frontmatter: &Value) -> Self {
        match frontmatter
            .get("backend_ref")
            .and_then(|b| b.get("kind"))
            .and_then(Value::as_str)
        {
            Some("sql_view") => Self::SqlView,
            Some("document") => Self::Document,
            Some("openapi") | Some("mcp") => Self::RemoteProxy,
            _ => Self::Native,
        }
    }

    /// Whether the backend keeps its bytes in a blob this server can serve.
    ///
    /// Only `document` does. `fetch_blob` used to ask this question by
    /// re-probing `backend_ref.kind` inline; asking the enum keeps the answer
    /// in one place.
    pub(super) fn has_fetchable_blob(self) -> bool {
        matches!(self, Self::Document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_every_known_kind() {
        let fm = |k: &str| json!({ "backend_ref": { "kind": k } });
        assert_eq!(BackendView::of(&fm("sql_view")), BackendView::SqlView);
        assert_eq!(BackendView::of(&fm("document")), BackendView::Document);
        assert_eq!(BackendView::of(&fm("openapi")), BackendView::RemoteProxy);
        assert_eq!(BackendView::of(&fm("mcp")), BackendView::RemoteProxy);
    }

    #[test]
    fn a_page_without_a_backend_ref_is_native() {
        assert_eq!(BackendView::of(&json!({})), BackendView::Native);
        assert_eq!(
            BackendView::of(&json!({ "skill": "customer" })),
            BackendView::Native
        );
    }

    #[test]
    fn an_unknown_kind_degrades_to_native_rather_than_erroring() {
        let fm = json!({ "backend_ref": { "kind": "some_future_backend" } });
        assert_eq!(BackendView::of(&fm), BackendView::Native);
    }

    #[test]
    fn only_documents_carry_a_fetchable_blob() {
        assert!(BackendView::Document.has_fetchable_blob());
        for v in [
            BackendView::Native,
            BackendView::SqlView,
            BackendView::RemoteProxy,
        ] {
            assert!(!v.has_fetchable_blob(), "{v:?} must not offer a blob");
        }
    }
}
