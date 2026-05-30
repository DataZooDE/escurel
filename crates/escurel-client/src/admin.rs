//! Typed gRPC client for the Escurel **admin** surface
//! (`escurel.v1.EscurelAdmin`).
//!
//! This is the operator counterpart to [`crate::Client`]: tenant
//! lifecycle, drift audit, quota inspection, and chat-history erasure.
//! It is a separate handle wrapping the generated
//! [`EscurelAdminClient`] because the admin service is a distinct gRPC
//! service from the agent surface — but the connect + secret-custody
//! pattern is identical to [`crate::Client`].
//!
//! Every RPC except [`AdminClient::health`] requires an **admin-role**
//! bearer token; the server rejects an agent-role token with
//! `PermissionDenied` (surfaced here as [`Error::Rpc`]). `health` is
//! the substrate liveness probe and works for any caller (and
//! unauthenticated in dev mode).
//!
//! The streaming admin RPCs (`tenant_export` / `tenant_import` /
//! `rebuild` / `compact_lanes`) are intentionally **not** on this
//! handle yet — they land with the streaming surface in a follow-up.

use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use secrecy::{ExposeSecret as _, SecretString};
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::Channel;

use crate::Error;

pub use escurel_proto::v1::{
    AttachExternalRequest, AttachExternalResponse, AuditRequest, AuditResponse,
    CompactLanesRequest, CompactProgress, DeleteChatHistoryRequest, DeleteChatHistoryResponse,
    EmbeddingReloadRequest, EmbeddingReloadResponse, HealthRequest, HealthResponse,
    QuotaGetRequest, QuotaGetResponse, RebuildProgress, RebuildRequest, TenantCreateRequest,
    TenantCreateResponse, TenantDeleteRequest, TenantDeleteResponse, TenantExportChunk,
    TenantExportRequest, TenantGetRequest, TenantGetResponse, TenantImportChunk,
    TenantImportResponse, TenantListRequest, TenantListResponse, TenantSpec, TenantUpdateRequest,
    TenantUpdateResponse,
};

/// Typed gRPC client for the Escurel v1 **admin** surface.
///
/// Opaque on purpose, exactly like [`crate::Client`]: the channel and
/// the bearer token are private; the bearer lives inside a
/// [`SecretString`] and is never returned by an accessor nor printed
/// in `Debug` output.
#[derive(Clone)]
pub struct AdminClient {
    inner: EscurelAdminClient<Channel>,
    bearer: MetadataValue<Ascii>,
    _token: SecretString,
}

impl std::fmt::Debug for AdminClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print `bearer` / `_token` — same leak invariant as
        // `crate::Client`.
        f.debug_struct("AdminClient").finish_non_exhaustive()
    }
}

impl AdminClient {
    /// Dial the gateway at `endpoint` (e.g. `http://127.0.0.1:8081`)
    /// and authenticate subsequent admin RPCs with `token`.
    ///
    /// Errors mirror [`crate::Client::connect`]:
    /// [`Error::InvalidEndpoint`], [`Error::InvalidToken`],
    /// [`Error::Connect`].
    pub async fn connect(endpoint: &str, token: SecretString) -> Result<Self, Error> {
        let bearer: MetadataValue<Ascii> = format!("Bearer {}", token.expose_secret())
            .parse()
            .map_err(|_| Error::InvalidToken)?;
        let channel = Channel::from_shared(endpoint.to_owned())
            .map_err(|_| Error::InvalidEndpoint(endpoint.to_owned()))?
            .connect()
            .await
            .map_err(Error::Connect)?;
        Ok(Self {
            inner: EscurelAdminClient::new(channel),
            bearer,
            _token: token,
        })
    }

    /// Substrate liveness probe — returns the gateway version. Works
    /// for any authenticated caller (admin role not required).
    pub async fn health(&self, req: HealthRequest) -> Result<HealthResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.health(self.authed(req)).await?.into_inner())
    }

    /// Create a tenant (provisions its directory + DuckDB file).
    pub async fn tenant_create(
        &self,
        req: TenantCreateRequest,
    ) -> Result<TenantCreateResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.tenant_create(self.authed(req)).await?.into_inner())
    }

    /// List all tenants.
    pub async fn tenant_list(&self, req: TenantListRequest) -> Result<TenantListResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.tenant_list(self.authed(req)).await?.into_inner())
    }

    /// Fetch one tenant's spec.
    pub async fn tenant_get(&self, req: TenantGetRequest) -> Result<TenantGetResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.tenant_get(self.authed(req)).await?.into_inner())
    }

    /// Update a tenant's spec (e.g. its display name).
    pub async fn tenant_update(
        &self,
        req: TenantUpdateRequest,
    ) -> Result<TenantUpdateResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.tenant_update(self.authed(req)).await?.into_inner())
    }

    /// Delete a tenant and its on-disk state.
    pub async fn tenant_delete(
        &self,
        req: TenantDeleteRequest,
    ) -> Result<TenantDeleteResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.tenant_delete(self.authed(req)).await?.into_inner())
    }

    /// Report drift between canonical markdown and the DuckDB index.
    pub async fn audit(&self, req: AuditRequest) -> Result<AuditResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.audit(self.authed(req)).await?.into_inner())
    }

    /// Snapshot a tenant's remaining quota budget.
    pub async fn quota_get(&self, req: QuotaGetRequest) -> Result<QuotaGetResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.quota_get(self.authed(req)).await?.into_inner())
    }

    /// GDPR erasure / retention prune of chat history. The
    /// `chat_group_id`, `before_ts` and `author` filters compose with
    /// AND; all empty means a full-tenant wipe.
    pub async fn delete_chat_history(
        &self,
        req: DeleteChatHistoryRequest,
    ) -> Result<DeleteChatHistoryResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client
            .delete_chat_history(self.authed(req))
            .await?
            .into_inner())
    }

    /// Attach an external read-only source to a tenant.
    pub async fn attach_external(
        &self,
        req: AttachExternalRequest,
    ) -> Result<AttachExternalResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.attach_external(self.authed(req)).await?.into_inner())
    }

    /// Hot-reload the embedding model.
    pub async fn embedding_reload(
        &self,
        req: EmbeddingReloadRequest,
    ) -> Result<EmbeddingReloadResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client
            .embedding_reload(self.authed(req))
            .await?
            .into_inner())
    }

    /// Rebuild a tenant's index, streaming one [`RebuildProgress`] per
    /// page (terminator chunk has `done == total`). Server-streaming.
    pub async fn rebuild(
        &self,
        req: RebuildRequest,
    ) -> Result<tonic::Streaming<RebuildProgress>, Error> {
        let mut client = self.inner.clone();
        Ok(client.rebuild(self.authed(req)).await?.into_inner())
    }

    /// Compact a tenant's CRDT op lanes, streaming a [`CompactProgress`]
    /// per page swept. Server-streaming; requires a CRDT backend.
    pub async fn compact_lanes(
        &self,
        req: CompactLanesRequest,
    ) -> Result<tonic::Streaming<CompactProgress>, Error> {
        let mut client = self.inner.clone();
        Ok(client.compact_lanes(self.authed(req)).await?.into_inner())
    }

    /// Export a tenant as a stream of tar+gz [`TenantExportChunk`]s.
    /// Server-streaming.
    pub async fn tenant_export(
        &self,
        req: TenantExportRequest,
    ) -> Result<tonic::Streaming<TenantExportChunk>, Error> {
        let mut client = self.inner.clone();
        Ok(client.tenant_export(self.authed(req)).await?.into_inner())
    }

    /// Import a tenant from a stream of tar+gz [`TenantImportChunk`]s.
    /// The first chunk must carry the target `tenant_id`. Client-
    /// streaming; returns the byte count once the stream is drained.
    pub async fn tenant_import<S>(&self, chunks: S) -> Result<TenantImportResponse, Error>
    where
        S: futures_core::Stream<Item = TenantImportChunk> + Send + 'static,
    {
        let mut client = self.inner.clone();
        Ok(client
            .tenant_import(self.authed(chunks))
            .await?
            .into_inner())
    }

    fn authed<T>(&self, body: T) -> tonic::Request<T> {
        let mut req = tonic::Request::new(body);
        req.metadata_mut()
            .insert("authorization", self.bearer.clone());
        req
    }
}
