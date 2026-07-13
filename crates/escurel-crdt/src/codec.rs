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

use loro::{ExportMode, LoroDoc, UpdateOptions};

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

/// CRDT three-way auto-merge of a whole-page markdown edit (#246).
///
/// Given the `base` snapshot the client branched from, the current
/// `head` markdown (concurrent server-side edits since `base`), and the
/// client's `incoming` markdown, produce the Loro-merged markdown that
/// carries **both** sides' changes.
///
/// How it works: both `head` and `incoming` derive from the same `base`
/// history. We fork the base into two docs (each fork gets a distinct
/// peer id, so their edits are *concurrent* rather than sequential),
/// diff each side's target text onto the shared base via
/// [`LoroText::update`] (Myers diff → minimal insert/delete ops), then
/// import one branch into the other. Loro unions the two op sets by op
/// id, interleaving concurrent edits deterministically — disjoint edits
/// (e.g. two different paragraphs) both survive.
///
/// This is a *text* merge: overlapping edits to the same region, or
/// edits that touch structured frontmatter, can interleave into markdown
/// that no longer parses. This function does **not** judge that — it
/// returns the merged string and the caller validates it (parses +
/// preserves the page's identity) before accepting it, falling back to a
/// conflict otherwise. The merged string is re-encoded through
/// [`snapshot_bytes_from_markdown`] for storage, so the ephemeral merge
/// peer ids never leak into a persisted snapshot.
///
/// # Errors
///
/// [`Error::Loro`] if `base` can't be imported, either diff can't be
/// applied (e.g. the Myers-diff timeout on a pathologically large text),
/// or the branch import fails.
pub fn three_way_merge(base: &[u8], head: &str, incoming: &str) -> Result<String, Error> {
    let base_doc = LoroDoc::new();
    base_doc.import(base)?;

    // Branch H: base -> head (the concurrent server-side edits).
    let doc_head = base_doc.fork();
    doc_head
        .set_peer_id(1)
        .map_err(|e| Error::Loro(format!("merge set head peer: {e}")))?;
    doc_head
        .get_text("body")
        .update(head, UpdateOptions::default())
        .map_err(|e| Error::Loro(format!("merge head diff: {e}")))?;
    doc_head.commit();

    // Branch I: base -> incoming (the client's edits). A distinct peer id
    // makes these ops concurrent with branch H's, so the union merges
    // instead of colliding on op ids.
    let doc_incoming = base_doc.fork();
    doc_incoming
        .set_peer_id(2)
        .map_err(|e| Error::Loro(format!("merge set incoming peer: {e}")))?;
    doc_incoming
        .get_text("body")
        .update(incoming, UpdateOptions::default())
        .map_err(|e| Error::Loro(format!("merge incoming diff: {e}")))?;
    doc_incoming.commit();

    // Union branch I's ops into branch H. Both share the base history, so
    // the Snapshot export is self-contained (no missing dependencies) and
    // Loro merges the divergent tails.
    doc_head.import(&doc_incoming.export(ExportMode::Snapshot)?)?;
    Ok(doc_head.get_text("body").to_string())
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

    const BASE: &str =
        "---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\nAlpha.\n\nBeta.\n\nGamma.\n";

    #[test]
    fn three_way_merge_keeps_both_disjoint_edits() {
        let base = snapshot_bytes_from_markdown(BASE).unwrap();
        // Server edited the first paragraph; client edited the last —
        // disjoint regions, so a clean CRDT merge keeps both.
        let head = BASE.replace("Alpha.", "Alpha EDITED-BY-SERVER.");
        let incoming = BASE.replace("Gamma.", "Gamma EDITED-BY-CLIENT.");

        let merged = three_way_merge(&base, &head, &incoming).unwrap();

        assert!(merged.contains("Alpha EDITED-BY-SERVER."), "{merged}");
        assert!(merged.contains("Gamma EDITED-BY-CLIENT."), "{merged}");
        assert!(
            merged.contains("Beta."),
            "untouched middle survives: {merged}"
        );
        // Frontmatter is untouched on both sides → merged page still parses
        // with its identity intact.
        let page = escurel_md::parse(&merged).expect("merged still parses");
        assert_eq!(
            page.frontmatter.fields.get("id").and_then(|v| v.as_str()),
            Some("c1")
        );
    }

    #[test]
    fn three_way_merge_is_deterministic() {
        // Fixed merge peer ids ⇒ the same three inputs always merge to the
        // same bytes (important for a reproducible server-side codec).
        let base = snapshot_bytes_from_markdown(BASE).unwrap();
        let head = BASE.replace("Alpha.", "A2.");
        let incoming = BASE.replace("Gamma.", "G2.");
        let a = three_way_merge(&base, &head, &incoming).unwrap();
        let b = three_way_merge(&base, &head, &incoming).unwrap();
        assert_eq!(a, b);
    }
}
