//! Held writes (`create_draft` / `list_drafts` / `promote_draft` /
//! `discard_draft`) — the wire types for the `autonomy: review` gate.
//!
//! A draft is a finished change that has NOT landed. It is deliberately not
//! a page: nothing that reads knowledge can return one, so an unapproved
//! change cannot be mistaken for knowledge. It is immutable — a revision is
//! a new draft — because a human approves specific bytes.

use serde::{Deserialize, Serialize};

use crate::null::null_as_default;

/// One held write, as the gateway stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Draft {
    pub draft_id: String,
    /// The page this write is FOR. It need not exist yet.
    pub target_page_id: String,
    /// The whole proposed markdown, frontmatter first.
    pub content: String,
    /// Hex sha256 of [`Self::content`] — the bytes an approval binds to.
    pub content_sha256: String,
    /// The target's hash when this was drafted; empty when drafted as a
    /// create (`null` on the wire).
    #[serde(deserialize_with = "null_as_default")]
    pub base_sha256: String,
    /// The subject that drafted it.
    pub author: String,
    /// The inbox event this answers, when it answers one (`null` on the
    /// wire otherwise).
    #[serde(deserialize_with = "null_as_default")]
    pub event_id: String,
    /// `open` | `promoted` | `discarded`.
    pub status: String,
    pub reason: String,
    pub decided_by: String,
    pub created_at: String,
}

/// `create_draft` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CreateDraftRequest {
    pub target_page_id: String,
    pub content: String,
    /// The target's `content_sha256` at drafting time; `Some("")` says "I
    /// expect no page yet". `None` drafts without a CAS, which promotion
    /// then performs unguarded — send it only when the target genuinely
    /// has no reader.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha256: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub event_id: String,
}

/// `create_draft` result: the stored draft, or the refusal that stopped it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CreateDraftResponse {
    pub ok: bool,
    pub draft: Option<Draft>,
    pub issues: Vec<crate::ValidationIssue>,
}

/// `list_drafts` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListDraftsRequest {
    /// `0` means the server's own default.
    pub limit: u32,
}

/// Everything still waiting, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListDraftsResponse {
    pub drafts: Vec<Draft>,
}

/// `promote_draft` / `discard_draft` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DecideDraftRequest {
    pub draft_id: String,
    /// Why it was refused. `discard_draft` only.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// The result of deciding a draft.
///
/// `promote_draft` answers with the underlying `update_page` envelope, so a
/// stale target reads as `{ok:false, issues:[{code:"conflict"}]}` — the same
/// shape, and the same `head_content`, an unguarded write would produce.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DecideDraftResponse {
    pub ok: bool,
    pub issues: Vec<crate::ValidationIssue>,
    /// Present on a conflict: the target's current bytes, to re-draft from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_content: Option<String>,
}

/// A branch, as registered (#512): an isolated workspace whose writes never
/// touch the base timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Branch {
    pub name: String,
    /// The corpus state the branch forked from — what a merge compares
    /// against.
    pub base_version: String,
    pub author: String,
    /// `open` | `merged` | `abandoned`.
    pub status: String,
    pub reason: String,
    pub decided_by: String,
    pub created_at: String,
}

/// `create_branch` / `merge_branch` / `abandon_branch` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BranchRequest {
    pub name: String,
    /// Why it was abandoned. `abandon_branch` only.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// `create_branch` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CreateBranchResponse {
    pub ok: bool,
    pub branch: Option<Branch>,
    pub issues: Vec<crate::ValidationIssue>,
}

/// `list_branches` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListBranchesResponse {
    pub branches: Vec<Branch>,
}

/// What one page did when a branch merged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BranchMergeResult {
    pub page_id: String,
    /// The base page the overlay landed on (or the delete applied to).
    pub target: String,
    /// This member was a tombstone, so merging it DELETES the base page.
    pub deleted: bool,
    pub ok: bool,
}

/// The outcome of deciding a branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DecideBranchResponse {
    pub ok: bool,
    pub name: String,
    pub results: Vec<BranchMergeResult>,
    /// A pre-flighted page refused mid-apply; re-running completes it.
    pub partial: bool,
    pub reason: String,
    pub issues: Vec<crate::ValidationIssue>,
}

/// `diff_draft` arguments (#509 §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiffDraftRequest {
    pub draft_id: String,
}

/// One frontmatter key that MOVES if the draft is approved. `from`/`to` are
/// `None` for an added / removed key respectively — the asymmetry is the
/// information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FrontmatterChange {
    pub key: String,
    pub from: Option<serde_json::Value>,
    pub to: Option<serde_json::Value>,
}

/// What happens to one body block. `kind` is `insert` | `delete` | `replace`;
/// `preview` is the PROPOSED text, because that is what is being approved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BlockChange {
    pub anchor: String,
    pub kind: String,
    pub preview: String,
}

/// What approving a held write would change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiffDraftResponse {
    pub ok: bool,
    pub draft_id: String,
    pub target_page_id: String,
    /// `false` = approving would CREATE the page.
    pub exists: bool,
    /// The target is not what it was when the draft was taken, so promotion
    /// is a merge rather than a write. Worth showing a reviewer BEFORE they
    /// approve, rather than as a failed promotion afterwards.
    pub base_moved: bool,
    pub frontmatter_changes: Vec<FrontmatterChange>,
    pub block_changes: Vec<BlockChange>,
    pub issues: Vec<crate::ValidationIssue>,
}
