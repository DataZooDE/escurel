//! Deterministic server-side Loro ↔ markdown codec (M7).
//!
//! A page's markdown (frontmatter + body) lives in a single Loro
//! `"body"` text container (see [`crate::livedoc`] / [`crate::reconciler`]).
//! These thin wrappers let the server **seed** a real snapshot history
//! for a page and **materialize** a historical snapshot back to markdown
//! — both without a Loro-aware client. Snapshot bytes are the only
//! deterministic server-authorable artefact (incremental ops are
//! peer-anchored; see
//! `docs/notes/discovered/2026-05-25-loro-incremental-updates-need-persistent-client.md`).

use loro::{ExportMode, LoroDoc};

use crate::error::Error;

/// Encode a full page markdown string into a Loro snapshot blob — the
/// inverse of [`body_from_snapshot`]. Mirrors the reconciler's
/// `snapshot_from_external` encode path.
pub fn snapshot_bytes_from_markdown(markdown: &str) -> Result<Vec<u8>, Error> {
    let doc = LoroDoc::new();
    doc.get_text("body").insert(0, markdown)?;
    doc.commit();
    Ok(doc.export(ExportMode::Snapshot)?)
}

/// Decode a Loro snapshot blob back to the page markdown it holds.
pub fn body_from_snapshot(snapshot: &[u8]) -> Result<String, Error> {
    let doc = LoroDoc::new();
    doc.import(snapshot)?;
    Ok(doc.get_text("body").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_full_page_markdown() {
        let md = "---\ntype: instance\nskill: engagement\nid: spine\ncontract_value: \"350k\"\n---\n# Spine\n\nBody.\n";
        let bytes = snapshot_bytes_from_markdown(md).unwrap();
        assert_eq!(body_from_snapshot(&bytes).unwrap(), md);
    }
}
