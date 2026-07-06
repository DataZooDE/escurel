//! Admin surface (proto `EscurelAdmin` + the admin-gated MCP tools).
//!
//! Field sets follow the proto. Where a live MCP admin tool diverges
//! (`admin_audit`, `admin_quota`, `admin_lane_*`, `admin_list_lanes`,
//! `admin_delete_chat_history`) the wire shape is noted on the type.

use serde::{Deserialize, Serialize};

// ── tenants ───────────────────────────────────────────────────────

/// Proto `TenantSpec`: `tenant_id`, `display_name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantSpec {
    pub tenant_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantCreateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<TenantSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantCreateResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<TenantSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantListRequest {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantListResponse {
    pub tenants: Vec<TenantSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantGetRequest {
    pub tenant_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantGetResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<TenantSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<TenantSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantUpdateResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<TenantSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantDeleteRequest {
    pub tenant_id: String,
    /// Confirmation token — the server requires it to equal `tenant_id` before
    /// performing the destructive delete. `None` ⇒ the server rejects the call.
    pub confirm: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantExportRequest {
    pub tenant_id: String,
}

/// Proto `TenantExportChunk`: `bytes data`. On an MCP/HTTP transport
/// this is base64-encoded; decoding to bytes is the consumer's job in
/// a later task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantExportChunk {
    pub data: Vec<u8>,
}

/// Proto `TenantImportChunk`: `tenant_id` + `bytes data`. Base64
/// handling deferred to the consumer (see `TenantExportChunk`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantImportChunk {
    pub tenant_id: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TenantImportResponse {
    pub bytes_imported: u64,
}

// ── audit / rebuild / attach / embedding / compact ────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuditRequest {
    pub tenant_id: String,
    pub scope: String,
}

/// MCP `admin_audit` wire keys: `markdown_not_in_duckdb`,
/// `indexed_but_no_markdown`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuditResponse {
    pub markdown_not_in_duckdb: Vec<String>,
    pub indexed_but_no_markdown: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RebuildRequest {
    pub tenant_id: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RebuildProgress {
    pub done: u64,
    pub total: u64,
    pub current_page: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AttachExternalRequest {
    pub tenant_id: String,
    pub source_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AttachExternalResponse {
    pub source_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EmbeddingReloadRequest {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EmbeddingReloadResponse {
    pub model_revision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CompactLanesRequest {
    pub tenant_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CompactProgress {
    pub ops_compacted: u64,
    pub bytes_reclaimed: u64,
}

// ── chat history / quota / health ─────────────────────────────────

/// `admin_delete_chat_history` arguments. Each filter is optional
/// (empty = no filter) and composes with AND.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DeleteChatHistoryRequest {
    pub tenant_id: String,
    pub chat_group_id: String,
    pub before_ts: String,
    pub author: String,
}

/// MCP `admin_delete_chat_history` wire key: `deleted`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DeleteChatHistoryResponse {
    pub deleted: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuotaGetRequest {
    pub tenant_id: String,
}

/// MCP `admin_quota` response. The live tool emits
/// `concurrent_sessions_in_use` (the in-use occupancy, not the proto's
/// `concurrent_sessions` cap), so the field serializes under that wire
/// key to stay byte-compatible with the dispatcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuotaGetResponse {
    pub queries_remaining: u32,
    pub writes_remaining: u32,
    pub embeds_remaining: u32,
    #[serde(rename = "concurrent_sessions_in_use")]
    pub concurrent_sessions: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HealthRequest {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

// ── admin lane introspection ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminListLanesRequest {}

/// One lane. MCP `admin_list_lanes` wire keys: `name`, `backend`,
/// `tenants_present`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LaneInfo {
    pub name: String,
    pub backend: String,
    pub tenants_present: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminListLanesResponse {
    pub lanes: Vec<LaneInfo>,
}

/// `admin_lane_keys` arguments. MCP wire keys: `lane`, `prefix`,
/// `limit`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminLaneKeysRequest {
    pub lane: String,
    pub prefix: String,
    pub limit: u32,
}

/// One key. MCP wire keys: `key`, `size_bytes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LaneKey {
    pub key: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminLaneKeysResponse {
    pub keys: Vec<LaneKey>,
}

/// `admin_lane_blob` arguments. MCP wire keys: `lane`, `key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminLaneBlobRequest {
    pub lane: String,
    pub key: String,
}

/// MCP `admin_lane_blob` response. The live tool emits the payload as
/// a base64 *string* under `bytes_base64`, plus `content_type` derived
/// from the key extension. The struct mirrors that wire exactly —
/// `bytes_base64` is the standard-alphabet base64 of the raw blob, and
/// decoding it back to bytes is the consumer's job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminLaneBlobResponse {
    pub bytes_base64: String,
    pub content_type: String,
}
