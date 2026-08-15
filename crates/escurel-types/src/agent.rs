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
    /// Block anchor of the hit. A page-grain hit (e.g. a `sql_view`
    /// candidate) has none — the wire emits an explicit `null`, which
    /// decodes to `""` here.
    #[serde(deserialize_with = "null_as_default")]
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
    /// Shadowed-base drift object (REQ-LAYER-03): when the expanded page
    /// is a tenant overlay skill shadowing a pack-imported base skill,
    /// the server emits `{base_page_id, pack, base}` here — the overlay
    /// wins for display, the base value stays visible. Absent otherwise
    /// (additive; old servers never emit it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Value>,
    /// The page's current monotonic version (`v<hlc>`, #246) — the value
    /// to send back as [`UpdatePageRequest::base_version`] in the
    /// read→edit→guarded-write cycle. Emitted only by a gateway with a
    /// live CRDT backend; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Hex sha256 of the STORED markdown bytes (#354/#408) — exactly what
    /// [`UpdatePageRequest::base_sha256`] compares against, closing the
    /// read→hash→guarded-write approve loop without a write-probe.
    /// Published on **plain reads only**: absent under `as_of`/`scenario`
    /// (a historical/overlaid body is not the current stored bytes) and
    /// on old servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// Backend overlay projection: for a `sql_view` instance the bounded
    /// rows + projected source columns (REQ-SQL-06/REQ-OV-02); for a
    /// remote (openapi/mcp) instance the LIVE upstream projection
    /// `{source, fields}` — or `{issue}` when the upstream failed.
    /// Absent for plain markdown pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_projection: Option<Value>,
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

// ── provenance graph (ADR-0010) ───────────────────────────────────

/// Bounded multi-hop ancestry over the provenance graph. `direction` is
/// `"up"` (everything this page rests on) or `"down"` (everything derived
/// from it); `relations` (possibly empty = all) restricts the edge kinds;
/// `max_hops` is clamped server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvenanceAncestryRequest {
    pub page_id: String,
    /// Target page: switches the tool to reachability/path mode
    /// (`ProvenancePathResponse` shape). Empty = the ancestry walk.
    pub to_page: String,
    pub direction: String,
    pub relations: Vec<String>,
    pub max_hops: u32,
    pub as_of: String,
}

/// One node reached while walking the provenance graph. MCP wire keys:
/// `page_id`, `skill`, `relation`, `depth`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvenanceHop {
    pub page_id: String,
    pub skill: String,
    // `null` for a bare/body link → "" rather than failing decode.
    #[serde(deserialize_with = "null_as_default")]
    pub relation: String,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvenanceAncestryResponse {
    pub hops: Vec<ProvenanceHop>,
}

/// `provenance_report` arguments: `kind` is `drift` or `abandoned`;
/// `skill` (empty = all) restricts the report. Consolidates the old
/// `expectation_drift` / `abandoned_paths` tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvenanceReportRequest {
    pub kind: String,
    pub skill: String,
}

/// One decision resting on a since-superseded expectation. MCP wire keys:
/// `decision_page_id`, `decision_skill`, `expectation_page_id`,
/// `superseding_page_id`, `decided_at`, `superseded_at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DriftRow {
    pub decision_page_id: String,
    pub decision_skill: String,
    pub expectation_page_id: String,
    pub superseding_page_id: String,
    pub decided_at: String,
    pub superseded_at: String,
}

/// `provenance_report` result. `rows` is kind-shaped: [`DriftRow`]s for
/// `kind: "drift"`, [`AbandonedNode`]s for `kind: "abandoned"` — kept as
/// raw JSON here so one response type serves both; decode into the row
/// type matching `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvenanceReportResponse {
    pub kind: String,
    pub rows: Value,
}

/// One node retired by supersession/abandonment. MCP wire keys:
/// `page_id`, `skill`, `via`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AbandonedNode {
    pub page_id: String,
    pub skill: String,
    pub via: String,
}

/// Path-mode arguments (the old `provenance_path` tool, now
/// `provenance_ancestry` with `to_page`; `from_page` binds via the
/// server-side alias). Shortest path from `from_page` to `to_page`
/// following `direction` (`up`/`down`), optionally restricted to
/// `relations`; `max_hops` clamped server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvenancePathRequest {
    pub from_page: String,
    pub to_page: String,
    pub direction: String,
    pub relations: Vec<String>,
    pub max_hops: u32,
}

/// `provenance_path` result: whether the target is reachable, and (if so)
/// the ordered page-id path and its hop count. When an interior node is
/// ACL-private the server returns `reachable: false` with an empty path
/// (no existence leak).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvenancePathResponse {
    pub reachable: bool,
    pub path: Vec<String>,
    pub depth: u32,
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
/// `visibility`, `owner_field`, `acl`, `backend`, `capabilities`, `params`.
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
    /// The skill page's stability layer (REQ-LAYER-04): `"overlay"` for a
    /// tenant-authored (editable) skill — the default — or
    /// `"base@<pack>@<version>"` for a skill imported from a subscribed
    /// pack, read-only at this node. Additive: absent on an old server ⇒
    /// `"overlay"`.
    #[serde(default = "default_layer")]
    pub layer: String,
    /// When this overlay skill shadows a pack-imported base skill of the
    /// same id (REQ-LAYER-03): the shadowed base's `base@<pack>@<version>`
    /// pin. Absent otherwise. The catalogue reports one entry per skill
    /// id — the overlay — so the pin is how an agent sees "this skill is a
    /// tenant specialisation of pack content".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<String>,
    /// The human-in-the-loop policy this skill declares (`autonomy:`):
    /// `"auto"` — a write derived from this skill commits directly;
    /// `"review"` — it is held for human approval; `"confirm"` — as review,
    /// plus an out-of-band notification. Escurel does not enforce the policy;
    /// it reports what the page declares, so a client can render the gate
    /// without expanding every skill.
    ///
    /// **Absent when the key is absent OR carries an unrecognised value.**
    /// Those two collapse deliberately: an unrecognised value must never
    /// arrive here as `"auto"`, because a consumer reading `auto` switches a
    /// human gate off. A client treats absence as "hold for review", and
    /// calls `validate` to learn which of the two cases it is.
    ///
    /// Typed as a string, not an enum, so a value added by a newer server
    /// still deserialises on an older client instead of failing the whole
    /// response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<String>,
    /// The parameters ONE RUN of this skill takes (`params:`, heron#11 /
    /// CR-7), in declaration order — enough for a client to build an input
    /// form from the catalogue alone, without expanding every page.
    ///
    /// Distinct from `required_frontmatter`, which is the shape of the
    /// INSTANCES this skill produces. The two nearly coincide for an
    /// instance-creating skill and diverge completely for a report skill
    /// parameterised by window and grouping.
    ///
    /// **Omitted from the wire when empty**, so a skill declaring no
    /// parameters — every skill that predates the key — has a byte-identical
    /// row, and an old client round-tripping one is never handed a field it
    /// did not send.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<SkillParam>,
}

/// One invocation parameter a skill declares. MCP wire keys: `name`,
/// `kind`, `required`, and the optional `label` / `description`.
///
/// The field set is exactly what an A2UI `form` field needs, and `kind` is
/// exactly its renderable set (`string` | `integer` | `boolean`), so the
/// surface renders with no mapping layer. Typed as a string, not an enum, so
/// a kind added by a newer server still deserialises on an older client
/// instead of failing the whole response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SkillParam {
    pub name: String,
    pub kind: String,
    pub required: bool,
    /// Human caption. `null`/absent ⇒ the client falls back to `name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The default page layer: tenant-authored, editable.
fn default_layer() -> String {
    "overlay".to_owned()
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
    /// Resume cursor from a previous response's
    /// [`ListInstancesResponse::next_cursor`]. Empty = start from the top.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cursor: String,
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
    /// Resume cursor: pass it back as [`ListInstancesRequest::cursor`] to
    /// fetch the next page. The wire keeps the key present (`null` on the
    /// last page) — **only absence/null means done**; an ACL filter may
    /// legitimately shorten a page below `limit` with more rows to come.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ── query_instance (issue #205) ───────────────────────────────────

/// `query_instance` arguments. MCP wire keys: `ref` (the query id or its
/// `[[query::id]]` wikilink, whose `target` names a sql_view instance) and
/// `params` (object; bound as prepared-statement values, never interpolated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QueryInstanceRequest {
    /// MCP wire key `ref` (a Rust keyword, hence the field rename).
    #[serde(rename = "ref")]
    pub query_ref: String,
    pub params: Value,
}

/// One projected result column. MCP wire key `type` (the proto field
/// was `type_name`). (Named for the retired stored-query surface it
/// debuted on; `query_instance` is its remaining consumer.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StoredQueryColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
}

/// MCP wire keys: `rows` (array), `schema` (columns), `truncated` (bool —
/// the result set hit the server row cap and was clipped).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QueryInstanceResponse {
    pub rows: Value,
    pub schema: Vec<StoredQueryColumn>,
    pub truncated: bool,
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

/// `update_page` arguments. MCP wire keys: `page_id`, `content`, plus the
/// optional concurrency/approve guards `base_version`,
/// `require_exact_base`, `base_sha256`, and the `provenance` passthrough.
///
/// Every guard is optional-with-meaning: **absent means unguarded** (the
/// wire semantics), so all of them are omitted from the serialized
/// arguments unless explicitly set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UpdatePageRequest {
    pub page_id: String,
    pub content: String,
    /// Optimistic-concurrency guard (#246): the page `version` the client
    /// last read (published by `expand` on a live-CRDT gateway, and by
    /// this tool's own `new_version`). Stale base → CRDT three-way
    /// auto-merge, or a typed `conflict` when unmergeable. On a gateway
    /// with no CRDT backend the guard refuses (`versioning_unavailable`)
    /// rather than being silently dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_version: Option<String>,
    /// Strict compare-and-swap: with a stale `base_version`, conflict
    /// outright instead of attempting the auto-merge — the
    /// human-in-the-loop approval shape. Requires `base_version`
    /// (the server rejects the flag without one).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_exact_base: bool,
    /// Content-hash compare-and-swap (#354) — the approve guard that works
    /// on EVERY gateway: the hex sha256 of the stored markdown the held
    /// write was drafted against, as published by `expand`'s
    /// `content_sha256`. `Some("")` is the **approve-create sentinel**
    /// ("I expect no page yet") and is serialized as the empty string;
    /// `None` means unguarded and is omitted from the wire. A mismatch
    /// refuses with `code: conflict` + `head_sha256` + `head_content`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha256: Option<String>,
    /// Provenance passthrough (#246): a runner-orchestrated write carries
    /// its `provenance.workflow`/`runner` block, which suppresses the
    /// opt-in `page-edited` event for that write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Value>,
}

/// MCP wire keys: `ok`, `issues`, `new_version`, `auto_merged`, and — on a
/// `conflict` refusal — `head_version` / `head_sha256` / `head_content`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UpdatePageResponse {
    pub ok: bool,
    pub issues: Vec<ValidationIssue>,
    pub new_version: String,
    /// The write landed as a CRDT three-way auto-merge of a stale
    /// `base_version` draft with the concurrent head (never `true` under
    /// `require_exact_base`). Absent on old servers ⇒ `false`.
    pub auto_merged: bool,
    /// On a `base_version` conflict: the head version the caller must
    /// re-read before re-drafting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_version: Option<String>,
    /// On a `base_sha256` conflict: the hash of the stored markdown at
    /// head (`""` when no page exists yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha256: Option<String>,
    /// On a conflict: the stored markdown at head, for the caller to
    /// re-diff / re-draft against. The wire may carry an explicit `null`
    /// (page absent), which decodes to `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_content: Option<String>,
}

/// `delete_page` arguments (#300). MCP wire keys: `page_id`, optional
/// `base_version` (optimistic-concurrency guard, empty = unguarded).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DeletePageRequest {
    pub page_id: String,
    pub base_version: String,
}

/// MCP wire keys: `ok`, `issues`, `page_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DeletePageResponse {
    pub ok: bool,
    pub issues: Vec<ValidationIssue>,
    pub page_id: String,
}

/// `purge_page`: permanently remove an already-archived page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PurgePageRequest {
    pub page_id: String,
}

/// MCP wire keys: `ok`, `issues`, `page_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PurgePageResponse {
    pub ok: bool,
    pub issues: Vec<ValidationIssue>,
    pub page_id: String,
}

/// `move_page`: rename a page id, leaving nothing at the old one.
///
/// Distinct from `delete_page`, which *retracts* and deliberately retains the
/// old markdown as an audit record. A move is not a retraction — the content
/// still exists, at the new id — so keeping a husk is noise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MovePageRequest {
    pub from: String,
    pub to: String,
}

/// MCP wire keys: `ok`, `issues`, `from`, `to`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MovePageResponse {
    pub ok: bool,
    pub issues: Vec<ValidationIssue>,
    pub from: String,
    pub to: String,
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

// ── fetch_blob ────────────────────────────────────────────────────

/// `fetch_blob` arguments. MCP wire key: `page_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FetchBlobRequest {
    pub page_id: String,
}

/// The retained original file of a `document`-backed instance. MCP wire
/// keys: `page_id`, `content_type`, `size`, `bytes_base64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BlobInfo {
    pub page_id: String,
    /// The MIME the upload declared (falling back to a byte sniff for
    /// overlays written before the field existed).
    pub content_type: String,
    /// Decoded size in bytes (server-capped per transfer).
    pub size: u64,
    /// The original bytes, base64-encoded.
    pub bytes_base64: String,
}

/// MCP wire key: `blob` — `null` for an absent page, a non-document page,
/// or a page the caller may not read (ONE indistinguishable answer; no
/// existence oracle). Decodes to `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FetchBlobResponse {
    pub blob: Option<BlobInfo>,
}

// ── list_snapshots / list_op_authors (CRDT history reads) ─────────

/// `list_snapshots` arguments. MCP wire key: `page_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListSnapshotsRequest {
    pub page_id: String,
}

/// MCP wire key: `snapshots` — the `taken_at` timestamps of the page's
/// CRDT snapshot history, oldest first: the discrete state-over-time
/// points `expand(as_of=T)` can replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListSnapshotsResponse {
    pub snapshots: Vec<String>,
}

/// `list_op_authors` arguments. MCP wire key: `page_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListOpAuthorsRequest {
    pub page_id: String,
}

/// One CRDT op's attribution. MCP wire keys: `op_id`, `hlc`,
/// `applied_at`, `principal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OpAuthor {
    pub op_id: String,
    /// Hybrid logical clock the op landed at (the version space
    /// `v<hlc>` is minted from).
    pub hlc: i64,
    /// RFC-3339-ish apply timestamp; `null` for pre-migration rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
    /// The **server-verified** principal that submitted the op — never
    /// the Loro peer id in the payload (a device, not a person; #357).
    /// `null` for ops applied before the gateway recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// MCP wire keys: `page_id`, `ops` (oldest first). A page the caller may
/// not read reports an empty history — indistinguishable from one that
/// has none (no existence oracle). No op bytes are ever returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListOpAuthorsResponse {
    pub page_id: String,
    pub ops: Vec<OpAuthor>,
}

// ── write_instance (remote proxy write-back) ──────────────────────

/// `write_instance` arguments. MCP wire keys: `ref` (a Rust keyword,
/// hence the field rename — the target instance id or its
/// `[[skill::id]]` wikilink) and `payload` (object forwarded to the
/// binding's upstream `write` op). Gated by the target instance's
/// `acl.update`, fail-closed; a binding with no `write` op is refused
/// (`backend_read_only`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WriteInstanceRequest {
    #[serde(rename = "ref")]
    pub instance_ref: String,
    pub payload: Value,
}

/// MCP wire keys: `ok`, `source` (the endpoint name written through),
/// `fields` (the re-projected upstream state after the write).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WriteInstanceResponse {
    pub ok: bool,
    pub source: String,
    pub fields: Value,
}
