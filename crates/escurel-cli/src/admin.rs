//! Operator-surface commands (the `EscurelAdmin` service). All require
//! an admin-role bearer except `health`. Streaming RPCs (`rebuild`,
//! `compact-lanes`, `tenant export`) drain to a collected JSON array;
//! `tenant import` streams a file's bytes up in one chunk.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use escurel_client::{
    AdminClient, AttachExternalRequest, AuditRequest, CompactLanesRequest,
    DeleteChatHistoryRequest, EmbeddingReloadRequest, HealthRequest, QuotaGetRequest,
    RebuildRequest, TenantCreateRequest, TenantDeleteRequest, TenantExportRequest,
    TenantGetRequest, TenantImportChunk, TenantListRequest, TenantSpec, TenantUpdateRequest,
};
use futures::StreamExt as _;
use serde_json::{Value, json};

use crate::convert::opt;

#[derive(Subcommand, Debug)]
pub enum AdminCmd {
    /// Substrate liveness probe + gateway version (no admin role).
    Health,
    /// Tenant lifecycle.
    #[command(subcommand)]
    Tenant(TenantCmd),
    /// Markdown ↔ DuckDB drift report.
    Audit {
        #[arg(long)]
        tenant: String,
        #[arg(long, default_value = "")]
        scope: String,
    },
    /// Per-tenant remaining quota snapshot.
    Quota {
        #[arg(long)]
        tenant: String,
    },
    /// GDPR erasure / retention prune of chat history. Empty filters
    /// compose with AND; all empty = full-tenant wipe.
    DeleteChatHistory(DeleteChatArgs),
    /// Attach an external read-only source to a tenant.
    AttachExternal {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        source_url: String,
    },
    /// Hot-reload the embedding model.
    EmbeddingReload,
    /// Rebuild a tenant's index (streams progress).
    Rebuild {
        #[arg(long)]
        tenant: String,
        #[arg(long, default_value = "")]
        scope: String,
    },
    /// Compact a tenant's CRDT op lanes (streams progress).
    CompactLanes {
        #[arg(long)]
        tenant: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TenantCmd {
    Create {
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "")]
        name: String,
    },
    List,
    Get {
        #[arg(long)]
        id: String,
    },
    Update {
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: String,
    },
    Delete {
        #[arg(long)]
        id: String,
    },
    /// Export a tenant as a tar+gz stream to a file.
    Export {
        #[arg(long)]
        id: String,
        /// Output file path for the tarball bytes.
        #[arg(long)]
        out: String,
    },
    /// Import a tenant from a tar+gz file.
    Import {
        #[arg(long)]
        id: String,
        /// Input tarball file path.
        #[arg(long, name = "in")]
        input: String,
    },
}

#[derive(Args, Debug)]
pub struct DeleteChatArgs {
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub group: Option<String>,
    #[arg(long)]
    pub before_ts: Option<String>,
    #[arg(long)]
    pub author: Option<String>,
}

fn tenant_json(s: Option<TenantSpec>) -> Value {
    match s {
        Some(s) => json!({ "tenant_id": s.tenant_id, "display_name": opt(&s.display_name) }),
        None => Value::Null,
    }
}

pub async fn run(client: &AdminClient, cmd: AdminCmd) -> Result<Value> {
    match cmd {
        AdminCmd::Health => {
            let r = client.health(HealthRequest::default()).await?;
            Ok(json!({ "status": r.status, "version": r.version }))
        }
        AdminCmd::Tenant(t) => tenant(client, t).await,
        AdminCmd::Audit { tenant, scope } => {
            let r = client
                .audit(AuditRequest {
                    tenant_id: tenant,
                    scope,
                })
                .await?;
            Ok(json!({
                "markdown_not_in_duckdb": r.markdown_not_in_duckdb,
                "indexed_but_no_markdown": r.indexed_but_no_markdown,
            }))
        }
        AdminCmd::Quota { tenant } => {
            let r = client
                .quota_get(QuotaGetRequest { tenant_id: tenant })
                .await?;
            Ok(json!({
                "queries_remaining": r.queries_remaining,
                "writes_remaining": r.writes_remaining,
                "embeds_remaining": r.embeds_remaining,
                "concurrent_sessions": r.concurrent_sessions,
            }))
        }
        AdminCmd::DeleteChatHistory(a) => {
            let r = client
                .delete_chat_history(DeleteChatHistoryRequest {
                    tenant_id: a.tenant,
                    chat_group_id: a.group.unwrap_or_default(),
                    before_ts: a.before_ts.unwrap_or_default(),
                    author: a.author.unwrap_or_default(),
                })
                .await?;
            Ok(json!({ "deleted": r.deleted }))
        }
        AdminCmd::AttachExternal { tenant, source_url } => {
            let r = client
                .attach_external(AttachExternalRequest {
                    tenant_id: tenant,
                    source_url,
                })
                .await?;
            Ok(json!({ "source_id": r.source_id }))
        }
        AdminCmd::EmbeddingReload => {
            let r = client
                .embedding_reload(EmbeddingReloadRequest::default())
                .await?;
            Ok(json!({ "model_revision": r.model_revision }))
        }
        AdminCmd::Rebuild { tenant, scope } => {
            let mut stream = client
                .rebuild(RebuildRequest {
                    tenant_id: tenant,
                    scope,
                })
                .await?;
            let mut progress = Vec::new();
            while let Some(msg) = stream.next().await {
                let p = msg?;
                progress.push(json!({
                    "done": p.done,
                    "total": p.total,
                    "current_page": opt(&p.current_page),
                }));
            }
            Ok(json!({ "progress": progress }))
        }
        AdminCmd::CompactLanes { tenant } => {
            let mut stream = client
                .compact_lanes(CompactLanesRequest { tenant_id: tenant })
                .await?;
            let mut progress = Vec::new();
            while let Some(msg) = stream.next().await {
                let p = msg?;
                progress.push(json!({
                    "ops_compacted": p.ops_compacted,
                    "bytes_reclaimed": p.bytes_reclaimed,
                }));
            }
            Ok(json!({ "progress": progress }))
        }
    }
}

async fn tenant(client: &AdminClient, cmd: TenantCmd) -> Result<Value> {
    match cmd {
        TenantCmd::Create { id, name } => {
            let r = client
                .tenant_create(TenantCreateRequest {
                    spec: Some(TenantSpec {
                        tenant_id: id,
                        display_name: name,
                    }),
                })
                .await?;
            Ok(json!({ "tenant": tenant_json(r.spec) }))
        }
        TenantCmd::List => {
            let r = client.tenant_list(TenantListRequest::default()).await?;
            Ok(json!({
                "tenants": r.tenants.into_iter().map(|s| json!({
                    "tenant_id": s.tenant_id,
                    "display_name": opt(&s.display_name),
                })).collect::<Vec<_>>(),
            }))
        }
        TenantCmd::Get { id } => {
            let r = client
                .tenant_get(TenantGetRequest { tenant_id: id })
                .await?;
            Ok(json!({ "tenant": tenant_json(r.spec) }))
        }
        TenantCmd::Update { id, name } => {
            let r = client
                .tenant_update(TenantUpdateRequest {
                    spec: Some(TenantSpec {
                        tenant_id: id,
                        display_name: name,
                    }),
                })
                .await?;
            Ok(json!({ "tenant": tenant_json(r.spec) }))
        }
        TenantCmd::Delete { id } => {
            let r = client
                .tenant_delete(TenantDeleteRequest { tenant_id: id })
                .await?;
            Ok(json!({ "deleted": r.deleted }))
        }
        TenantCmd::Export { id, out } => {
            let mut stream = client
                .tenant_export(TenantExportRequest { tenant_id: id })
                .await?;
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                bytes.extend_from_slice(&chunk?.data);
            }
            let n = bytes.len();
            std::fs::write(&out, &bytes).with_context(|| format!("write export to {out}"))?;
            Ok(json!({ "bytes_exported": n, "path": out }))
        }
        TenantCmd::Import { id, input } => {
            let bytes =
                std::fs::read(&input).with_context(|| format!("read import from {input}"))?;
            let chunks = futures::stream::iter(vec![TenantImportChunk {
                tenant_id: id,
                data: bytes,
            }]);
            let r = client.tenant_import(chunks).await?;
            Ok(json!({ "bytes_imported": r.bytes_imported }))
        }
    }
}
