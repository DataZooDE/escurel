//! Agent-facing tool request/response types: search, resolve, expand,
//! neighbours, skills, instances, stored queries, validate, update.
//!
//! Field sets follow the MCP wire contract (`escurel-server/src/mcp.rs`
//! `json!` builders + `*Args` structs, and the `escurel-test-support`
//! `decode_*` helpers). The MCP-over-HTTP transport is the contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::{PageRef, WikilinkParsed};
use crate::null::null_as_default;

// ── search ────────────────────────────────────────────────────────

/// `search` tool arguments. MCP wire keys: `q`, `k`, `page_type`,
/// `skill`, `granularity`, `filter`, `as_of`, `scenario`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SearchRequest {
    pub q: String,
    pub k: u32,
    pub granularity: String,
    pub page_type: String,
    pub skill: String,
    /// Frontmatter post-filter (MCP `filter` object). Proto carried a
    /// `filter_json` string; the wire is a real JSON object.
    pub filter: Value,
    pub as_of: String,
    pub scenario: String,
    /// Restrict the search to a single page's blocks (relevance heatmap).
    /// Empty = no restriction.
    pub page_id: String,
}

/// One block-granularity hit. MCP wire keys: `page_id`, `slug`,
/// `skill`, `page_type`, `anchor`, `snippet`, `score`,
/// `frontmatter_excerpt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SearchHit {
    pub page_id: String,
    pub slug: String,
    pub skill: String,
    pub page_type: String,
    pub anchor: String,
    pub snippet: String,
    pub score: f64,
    /// Absolute vector cosine similarity to the query (0..1); 0 for BM25-only
    /// hits. Honest relevance signal, independent of the RRF rank.
    pub similarity: f64,
    /// MCP wire key `frontmatter_excerpt` carries a real JSON object
    /// (the proto encoded this as the string `frontmatter_excerpt_json`).
    pub frontmatter_excerpt: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// Present on the MCP wire (proto has it too).
    pub granularity: String,
}

// ── resolve ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ResolveRequest {
    pub wikilink: String,
    pub scenario: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ResolveResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<WikilinkParsed>,
    /// Absent when the wikilink could not be resolved (MCP wire emits
    /// `null`; we model it as `Option`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<PageRef>,
    pub exists: bool,
}

// ── expand ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExpandRequest {
    pub page_id: String,
    pub anchor: String,
    pub version: String,
    pub as_of: String,
    pub scenario: String,
    /// Return ALL chunks of a document instance (detail/heatmap view) instead
    /// of the bounded lead. Default `false` (grounding/preview).
    pub full: bool,
}

/// One body block. MCP wire keys: `anchor`, `content`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExpandBlock {
    pub anchor: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExpandResponse {
    /// `null` on the MCP wire when the page does not exist / is
    /// time-travelled out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<PageRef>,
    /// MCP wire key `frontmatter` carries a real JSON object (the proto
    /// encoded this as the string `frontmatter_json`).
    pub frontmatter: Value,
    pub body: String,
    pub blocks: Vec<ExpandBlock>,
    pub wikilinks_out: Vec<WikilinkParsed>,
}

// ── neighbours ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NeighboursRequest {
    pub page_id: String,
    pub direction: String,
    pub link_skill: String,
    pub as_of: String,
    pub scenario: String,
}

/// One edge. Mirrors escurel-index `Edge`. MCP wire keys: `src_page`,
/// `dst_page`, `link_skill`, `link_version`, `dst_anchor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Edge {
    pub src_page: String,
    pub dst_page: String,
    pub link_skill: String,
    // The MCP wire emits `null` for an edge with no pinned version /
    // no destination anchor; map it to "" rather than failing decode.
    #[serde(deserialize_with = "null_as_default")]
    pub link_version: String,
    #[serde(deserialize_with = "null_as_default")]
    pub dst_anchor: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NeighboursResponse {
    pub edges: Vec<Edge>,
}

// ── skills / instances ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListSkillsRequest {}

/// The per-CRUD group ACL a skill declares (the nested `acl:` block, or
/// the policy a legacy `visibility:` field maps to). Each verb is a list
/// of group names; an omitted verb (`null`) falls through to the tenant
/// default at decision time. Reported additively alongside the legacy
/// `visibility`/`owner_field` keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillAcl {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<Vec<String>>,
}

/// The backend a skill's instances live in (`markdown` | `sql_view` |
/// `document`). Additive on the `list_skills` wire surface so a client can
/// tell which backend a `[[skill::id]]` resolves to. Absent `backend:`
/// block ⇒ `kind: "markdown"` (every skill today).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillBackend {
    pub kind: String,
}

/// What a skill's backend can do — reported so a client learns
/// read-only-ness, granularity, and search mode without a second call
/// (REQ-BK-02). Additive; old clients ignore it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillCapabilities {
    /// Instances can be created / overwritten via `update_page`.
    pub writable: bool,
    /// Finest addressable unit (`block` | `page`).
    pub granularity: String,
    /// How this backend contributes to search (`hybrid` | …).
    pub search: String,
    /// Whether CRDT co-authoring applies to its pages.
    pub supports_crdt: bool,
}

/// A Tier-1 skill. MCP wire keys: `id`, `description`,
/// `required_frontmatter`, `optional_frontmatter`, `is_event_typed`,
/// `visibility`, `owner_field`, `acl`, `backend`, `capabilities`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Skill {
    pub id: String,
    pub description: String,
    pub required_frontmatter: Vec<String>,
    pub optional_frontmatter: Vec<String>,
    pub is_event_typed: bool,
    /// Read policy this skill declares (`public` | `owner`). Lets a
    /// consumer (e.g. the explorer's edit gate) tell operator-editable
    /// public skills from owner-bound ones without a second call. Retained
    /// as a derived convenience for old clients; `acl` is the full model.
    pub visibility: String,
    /// The frontmatter field naming the owning principal, when
    /// `visibility` is `owner` (else `null`). An owner-bound skill is not
    /// operator-editable.
    pub owner_field: Option<String>,
    /// The resolved per-CRUD group ACL (group ACL v1), or `null` when the
    /// skill declares neither an `acl:` block nor a legacy `visibility:`
    /// field (→ tenant default applies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acl: Option<SkillAcl>,
    /// The backend a skill's instances live in (markdown today).
    pub backend: SkillBackend,
    /// The backend's capability descriptor.
    pub capabilities: SkillCapabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListSkillsResponse {
    pub skills: Vec<Skill>,
}

/// `list_instances` arguments. MCP wire key for the skill is
/// `skill_id`; the proto field is `skill`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListInstancesRequest {
    #[serde(rename = "skill_id")]
    pub skill: String,
    pub order_by_at: String,
    pub limit: u32,
    pub frontmatter_key: String,
    pub frontmatter_value: String,
    pub as_of: String,
    pub scenario: String,
}

/// One instance row. MCP wire keys: `page_id`, `skill`,
/// `frontmatter` (object), `at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InstanceInfo {
    pub page_id: String,
    pub skill: String,
    /// MCP wire key `frontmatter` carries a real JSON object (the proto
    /// encoded this as the string `frontmatter_json`).
    pub frontmatter: Value,
    /// `null` on the wire when the instance carries no `at` timestamp.
    #[serde(deserialize_with = "null_as_default")]
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListInstancesResponse {
    pub instances: Vec<InstanceInfo>,
    /// MCP wire emits `next_cursor` (null today); pagination is not
    /// yet implemented server-side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ── stored queries ────────────────────────────────────────────────

/// `run_stored_query` arguments. MCP wire keys: `query_id`, `params`
/// (object). Proto used `query_id` + `params_json` (string).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RunStoredQueryRequest {
    pub query_id: String,
    /// MCP wire key `params` carries a real JSON object (the proto
    /// encoded this as the string `params_json`).
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StoredQueryColumn {
    pub name: String,
    /// MCP wire key `type` (the proto field is `type_name`).
    #[serde(rename = "type")]
    pub type_name: String,
}

/// MCP wire keys: `rows` (array), `schema` (array of columns). Proto
/// used `rows_json` (string) + `schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RunStoredQueryResponse {
    /// MCP wire key `rows` carries a real JSON array (the proto encoded
    /// this as the string `rows_json`).
    pub rows: Value,
    pub schema: Vec<StoredQueryColumn>,
}

// ── validate ──────────────────────────────────────────────────────

/// `validate` arguments. MCP wire keys: `content`, `as_page_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ValidateRequest {
    pub content: String,
    pub as_page_id: String,
}

/// One issue. MCP wire keys: `severity`, `code`, `location`,
/// `message`, optional `suggestion`. (The proto `ValidationIssue`
/// carries `code`/`message`/`anchor`; the live MCP shape uses
/// `location` + an optional `suggestion`, per `issue_to_json`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ValidationIssue {
    pub severity: String,
    pub code: String,
    pub location: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ValidateResponse {
    pub ok: bool,
    pub issues: Vec<ValidationIssue>,
}

// ── update / live ─────────────────────────────────────────────────

/// `update_page` arguments. MCP wire keys: `page_id`, `content`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UpdatePageRequest {
    pub page_id: String,
    pub content: String,
}

/// MCP wire keys: `ok`, `issues`, `new_version`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UpdatePageResponse {
    pub ok: bool,
    pub issues: Vec<ValidationIssue>,
    pub new_version: String,
}

// ── outbound webhook delivery log ─────────────────────────────────

/// One outbound-webhook delivery outcome (group ACL-independent
/// observability). MCP wire keys: `event_id`, `at_ms`, `ok`,
/// `http_status`, `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebhookDelivery {
    pub event_id: String,
    /// Unix-millis timestamp of the delivery outcome.
    pub at_ms: u64,
    pub ok: bool,
    /// HTTP status code when a response was received; `null` on a
    /// transport error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// Transport/error detail when the POST failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `admin_webhook_deliveries` response: recent outbound-webhook delivery
/// outcomes (newest first), and whether a webhook URL is configured at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebhookDeliveriesResponse {
    /// Whether `ESCUREL_WEBHOOK_URL` is set. When false, `deliveries` is
    /// empty because nothing is ever sent.
    pub configured: bool,
    pub deliveries: Vec<WebhookDelivery>,
}
