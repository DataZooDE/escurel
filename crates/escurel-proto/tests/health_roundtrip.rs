//! End-to-end smoke test: real tonic server bound to a random TCP
//! port, real tonic client dialling back, calls `Health` on the
//! generated `EscurelAdmin` service. No mocks, no in-process
//! channel — proves the proto crate's codegen, server stub, and
//! client stub all line up.
//!
//! The other admin RPCs are stubbed with `Status::unimplemented`
//! so the trait is satisfied; their real bodies land in M3.5c.

use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use escurel_proto::v1::escurel_admin_server::{EscurelAdmin, EscurelAdminServer};
use escurel_proto::v1::{
    AdminLaneBlobRequest, AdminLaneBlobResponse, AdminLaneKeysRequest, AdminLaneKeysResponse,
    AdminListLanesRequest, AdminListLanesResponse, AttachExternalRequest, AttachExternalResponse,
    AuditRequest, AuditResponse, CompactLanesRequest, CompactProgress, DeleteChatHistoryRequest,
    DeleteChatHistoryResponse, EmbeddingReloadRequest, EmbeddingReloadResponse, HealthRequest,
    HealthResponse, QuotaGetRequest, QuotaGetResponse, RebuildProgress, RebuildRequest,
    TenantCreateRequest, TenantCreateResponse, TenantDeleteRequest, TenantDeleteResponse,
    TenantExportChunk, TenantExportRequest, TenantGetRequest, TenantGetResponse, TenantImportChunk,
    TenantImportResponse, TenantListRequest, TenantListResponse, TenantUpdateRequest,
    TenantUpdateResponse,
};
use futures::Stream;
use tokio::net::TcpListener;
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

#[derive(Default)]
struct StaticAdmin;

type EmptyStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl EscurelAdmin for StaticAdmin {
    async fn health(
        &self,
        _req: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            status: "ok".to_owned(),
            version: "1.0.0-test".to_owned(),
        }))
    }

    async fn tenant_create(
        &self,
        _req: Request<TenantCreateRequest>,
    ) -> Result<Response<TenantCreateResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }
    async fn tenant_list(
        &self,
        _req: Request<TenantListRequest>,
    ) -> Result<Response<TenantListResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }
    async fn tenant_get(
        &self,
        _req: Request<TenantGetRequest>,
    ) -> Result<Response<TenantGetResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }
    async fn tenant_update(
        &self,
        _req: Request<TenantUpdateRequest>,
    ) -> Result<Response<TenantUpdateResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }
    async fn tenant_delete(
        &self,
        _req: Request<TenantDeleteRequest>,
    ) -> Result<Response<TenantDeleteResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }

    type TenantExportStream = EmptyStream<TenantExportChunk>;
    async fn tenant_export(
        &self,
        _req: Request<TenantExportRequest>,
    ) -> Result<Response<Self::TenantExportStream>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }
    async fn tenant_import(
        &self,
        _req: Request<Streaming<TenantImportChunk>>,
    ) -> Result<Response<TenantImportResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }

    async fn audit(&self, _req: Request<AuditRequest>) -> Result<Response<AuditResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }

    type RebuildStream = EmptyStream<RebuildProgress>;
    async fn rebuild(
        &self,
        _req: Request<RebuildRequest>,
    ) -> Result<Response<Self::RebuildStream>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }

    async fn attach_external(
        &self,
        _req: Request<AttachExternalRequest>,
    ) -> Result<Response<AttachExternalResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }
    async fn embedding_reload(
        &self,
        _req: Request<EmbeddingReloadRequest>,
    ) -> Result<Response<EmbeddingReloadResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }

    type CompactLanesStream = EmptyStream<CompactProgress>;
    async fn compact_lanes(
        &self,
        _req: Request<CompactLanesRequest>,
    ) -> Result<Response<Self::CompactLanesStream>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }

    async fn quota_get(
        &self,
        _req: Request<QuotaGetRequest>,
    ) -> Result<Response<QuotaGetResponse>, Status> {
        Err(Status::unimplemented("M3.5c"))
    }

    async fn delete_chat_history(
        &self,
        _req: Request<DeleteChatHistoryRequest>,
    ) -> Result<Response<DeleteChatHistoryResponse>, Status> {
        Err(Status::unimplemented("M-Chat.3"))
    }

    async fn admin_list_lanes(
        &self,
        _req: Request<AdminListLanesRequest>,
    ) -> Result<Response<AdminListLanesResponse>, Status> {
        Err(Status::unimplemented("lane introspection"))
    }

    async fn admin_lane_keys(
        &self,
        _req: Request<AdminLaneKeysRequest>,
    ) -> Result<Response<AdminLaneKeysResponse>, Status> {
        Err(Status::unimplemented("lane introspection"))
    }

    async fn admin_lane_blob(
        &self,
        _req: Request<AdminLaneBlobRequest>,
    ) -> Result<Response<AdminLaneBlobResponse>, Status> {
        Err(Status::unimplemented("lane introspection"))
    }
}

#[tokio::test]
async fn admin_health_round_trips_over_real_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        Server::builder()
            .add_service(EscurelAdminServer::new(StaticAdmin))
            .serve_with_incoming_shutdown(incoming, async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = escurel_proto::v1::escurel_admin_client::EscurelAdminClient::new(channel);
    let resp = client
        .health(HealthRequest::default())
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.status, "ok");
    assert_eq!(resp.version, "1.0.0-test");

    shutdown_tx.send(()).unwrap();
    server_task.await.unwrap();
}
