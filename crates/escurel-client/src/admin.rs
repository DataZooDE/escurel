//! Typed MCP-over-HTTP client for the Escurel **admin** surface.
//!
//! This is the operator counterpart to [`crate::Client`]: tenant
//! lifecycle, drift audit, quota inspection, chat-history erasure, the
//! external-source / embedding-reload knobs, and the long-running ops
//! (`rebuild` / `compact_lanes` / tenant export+import). It speaks the
//! same MCP-over-HTTP transport as `Client` — the admin tools live on
//! the same `POST /mcp` endpoint, gated by an **admin-role** bearer.
//!
//! Every method except [`AdminClient::health`] requires an admin-role
//! token; the server returns the JSON-RPC error code
//! [`crate::JSONRPC_ADMIN_REQUIRED`] (`-32001`) when an agent-role
//! token calls one — surfaced here as [`Error::JsonRpc`]. `health` is
//! the substrate liveness probe and works for any caller (and
//! unauthenticated in dev mode).
//!
//! ## Long-op return shapes
//!
//! The old gRPC surface streamed progress; the MCP transport is
//! one-shot, so these methods return the *terminal* result:
//! - [`AdminClient::rebuild`] → [`RebuildProgress`] (`{done, total}`).
//! - [`AdminClient::compact_lanes`] → [`CompactProgress`]
//!   (`{ops_compacted, bytes_reclaimed}`).
//! - [`AdminClient::tenant_export`] → `Vec<u8>` (the decoded tarball).
//! - [`AdminClient::tenant_import`] → `u64` (bytes imported).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::error::Error;
use crate::transport::McpTransport;

pub use escurel_types::{
    AttachExternalRequest, AttachExternalResponse, AuditRequest, AuditResponse,
    CompactLanesRequest, CompactProgress, DeleteChatHistoryRequest, DeleteChatHistoryResponse,
    EmbeddingReloadRequest, EmbeddingReloadResponse, HealthRequest, HealthResponse,
    QuotaGetRequest, QuotaGetResponse, RebuildProgress, RebuildRequest, TenantCreateRequest,
    TenantCreateResponse, TenantDeleteRequest, TenantDeleteResponse, TenantExportRequest,
    TenantGetRequest, TenantGetResponse, TenantImportResponse, TenantListRequest,
    TenantListResponse, TenantUpdateRequest, TenantUpdateResponse,
};

/// Typed MCP-over-HTTP client for the Escurel v1 **admin** surface.
///
/// Opaque on purpose, exactly like [`crate::Client`]: the transport
/// and the bearer token are private; the bearer lives inside a
/// [`SecretString`] and is never returned by an accessor nor printed
/// in `Debug` output.
#[derive(Clone)]
pub struct AdminClient {
    transport: McpTransport,
}

impl std::fmt::Debug for AdminClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the bearer — same leak invariant as `crate::Client`.
        f.debug_struct("AdminClient").finish_non_exhaustive()
    }
}

impl AdminClient {
    /// Dial the gateway at `endpoint` (the HTTP base, e.g.
    /// `http://127.0.0.1:8080`) and authenticate subsequent admin tool
    /// calls with `token`.
    ///
    /// Errors mirror [`crate::Client::connect`]:
    /// [`Error::InvalidEndpoint`], [`Error::InvalidToken`].
    pub async fn connect(endpoint: &str, token: SecretString) -> Result<Self, Error> {
        Ok(Self {
            transport: McpTransport::new(endpoint, token)?,
        })
    }

    /// Substrate liveness probe. The MCP surface has no `health` tool;
    /// the gateway answers `GET /healthz` (dependency-free) and
    /// `GET /version`. This method probes both and synthesizes the
    /// response so the operator surface keeps its shape.
    pub async fn health(&self, _req: HealthRequest) -> Result<HealthResponse, Error> {
        let version = self.transport.get_text("/version").await?;
        Ok(HealthResponse {
            status: "ok".to_owned(),
            version: version.trim().to_owned(),
        })
    }

    /// Create a tenant (provisions its directory + DuckDB file).
    pub async fn tenant_create(
        &self,
        req: TenantCreateRequest,
    ) -> Result<TenantCreateResponse, Error> {
        let spec = req.spec.unwrap_or_default();
        self.transport
            .call_typed(
                "tenant_create",
                json!({ "tenant_id": spec.tenant_id, "display_name": spec.display_name }),
            )
            .await
    }

    /// List all tenants.
    pub async fn tenant_list(&self, _req: TenantListRequest) -> Result<TenantListResponse, Error> {
        self.transport.call_typed("tenant_list", json!({})).await
    }

    /// Fetch one tenant's spec.
    pub async fn tenant_get(&self, req: TenantGetRequest) -> Result<TenantGetResponse, Error> {
        self.transport
            .call_typed("tenant_get", json!({ "tenant_id": req.tenant_id }))
            .await
    }

    /// Update a tenant's spec (e.g. its display name).
    pub async fn tenant_update(
        &self,
        req: TenantUpdateRequest,
    ) -> Result<TenantUpdateResponse, Error> {
        let spec = req.spec.unwrap_or_default();
        self.transport
            .call_typed(
                "tenant_update",
                json!({ "tenant_id": spec.tenant_id, "display_name": spec.display_name }),
            )
            .await
    }

    /// Delete a tenant and its on-disk state.
    pub async fn tenant_delete(
        &self,
        req: TenantDeleteRequest,
    ) -> Result<TenantDeleteResponse, Error> {
        self.transport
            .call_typed(
                "tenant_delete",
                json!({ "tenant_id": req.tenant_id, "confirm": req.confirm }),
            )
            .await
    }

    /// Report drift between canonical markdown and the DuckDB index.
    pub async fn audit(&self, req: AuditRequest) -> Result<AuditResponse, Error> {
        let mut args = json!({ "tenant_id": req.tenant_id });
        if !req.scope.is_empty() {
            args["scope"] = json!(req.scope);
        }
        self.transport.call_typed("admin_audit", args).await
    }

    /// Snapshot a tenant's remaining quota budget.
    pub async fn quota_get(&self, req: QuotaGetRequest) -> Result<QuotaGetResponse, Error> {
        self.transport
            .call_typed("admin_quota", json!({ "tenant_id": req.tenant_id }))
            .await
    }

    /// GDPR erasure / retention prune of chat history. The
    /// `chat_group_id`, `before_ts` and `author` filters compose with
    /// AND; all empty means a full-tenant wipe.
    pub async fn delete_chat_history(
        &self,
        req: DeleteChatHistoryRequest,
    ) -> Result<DeleteChatHistoryResponse, Error> {
        let mut args = json!({ "tenant_id": req.tenant_id });
        if !req.chat_group_id.is_empty() {
            args["chat_group_id"] = json!(req.chat_group_id);
        }
        if !req.before_ts.is_empty() {
            args["before_ts"] = json!(req.before_ts);
        }
        if !req.author.is_empty() {
            args["author"] = json!(req.author);
        }
        self.transport
            .call_typed("admin_delete_chat_history", args)
            .await
    }

    /// Attach an external read-only source to the gateway's tenant. The
    /// server validates `tenant_id` against the gateway, derives a safe
    /// catalog alias from `source_url`, and returns it as `source_id`.
    pub async fn attach_external(
        &self,
        req: AttachExternalRequest,
    ) -> Result<AttachExternalResponse, Error> {
        self.transport
            .call_typed(
                "attach_external",
                json!({ "tenant_id": req.tenant_id, "source_url": req.source_url }),
            )
            .await
    }

    /// Hot-reload the embedding model.
    pub async fn embedding_reload(
        &self,
        _req: EmbeddingReloadRequest,
    ) -> Result<EmbeddingReloadResponse, Error> {
        self.transport
            .call_typed("embedding_reload", json!({}))
            .await
    }

    /// Rebuild a tenant's index. The MCP transport returns the
    /// terminal `{done, total}` rather than a progress stream.
    pub async fn rebuild(&self, req: RebuildRequest) -> Result<RebuildProgress, Error> {
        let mut args = json!({});
        if !req.tenant_id.is_empty() {
            args["tenant_id"] = json!(req.tenant_id);
        }
        if !req.scope.is_empty() {
            args["scope"] = json!(req.scope);
        }
        self.transport.call_typed("rebuild", args).await
    }

    /// Compact a tenant's CRDT op lanes. Returns the terminal
    /// `{ops_compacted, bytes_reclaimed}`.
    pub async fn compact_lanes(&self, req: CompactLanesRequest) -> Result<CompactProgress, Error> {
        self.transport
            .call_typed("compact_lanes", json!({ "tenant_id": req.tenant_id }))
            .await
    }

    /// Export a tenant as a tar+gz archive. The MCP tool returns the
    /// tarball base64-encoded under `tarball_b64`; this method decodes
    /// it and hands back the raw bytes.
    pub async fn tenant_export(&self, req: TenantExportRequest) -> Result<Vec<u8>, Error> {
        let result = self
            .transport
            .call("tenant_export", json!({ "tenant_id": req.tenant_id }))
            .await?;
        let b64 = result
            .get("tarball_b64")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Decode("tenant_export: missing `tarball_b64`".to_owned()))?;
        B64.decode(b64.as_bytes())
            .map_err(|e| Error::Decode(format!("tenant_export: tarball_b64 not base64: {e}")))
    }

    /// Import a tenant from tar+gz `bytes`. The bytes are base64-encoded
    /// for the MCP wire; returns the number of bytes imported.
    pub async fn tenant_import(&self, tenant_id: &str, bytes: Vec<u8>) -> Result<u64, Error> {
        let resp: TenantImportResponse = self
            .transport
            .call_typed(
                "tenant_import",
                json!({ "tenant_id": tenant_id, "tarball_b64": B64.encode(&bytes) }),
            )
            .await?;
        Ok(resp.bytes_imported)
    }

    /// Low-level escape hatch: call an arbitrary MCP tool and get the
    /// raw `result` JSON value back.
    pub async fn call_raw(&self, tool: &str, arguments: Value) -> Result<Value, Error> {
        self.transport.call(tool, arguments).await
    }
}
