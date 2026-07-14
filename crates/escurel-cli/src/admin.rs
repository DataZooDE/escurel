//! Operator-surface commands (the admin-gated MCP tools). All require
//! an admin-role bearer except `health`. The long-running ops
//! (`rebuild`, `compact-lanes`, `tenant export`/`import`) are one-shot
//! over the MCP transport: they return the terminal result directly
//! rather than a progress stream.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use escurel_client::{
    AdminClient, AttachExternalRequest, AuditRequest, CompactLanesRequest,
    DeleteChatHistoryRequest, EmbeddingReloadRequest, ExportPackRequest, HealthRequest,
    QuotaGetRequest, RebuildRequest, TenantCreateRequest, TenantDeleteRequest, TenantExportRequest,
    TenantGetRequest, TenantListRequest, TenantSpec, TenantUpdateRequest,
};
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
    /// Rebuild a tenant's index. Returns the terminal `{done, total}`.
    Rebuild {
        #[arg(long)]
        tenant: String,
        #[arg(long, default_value = "")]
        scope: String,
    },
    /// Compact a tenant's CRDT op lanes. Returns the terminal
    /// `{ops_compacted, bytes_reclaimed}`.
    CompactLanes {
        #[arg(long)]
        tenant: String,
    },
    /// Skill packs — the versioned, signed unit of distribution
    /// between escurel nodes.
    #[command(subcommand)]
    Pack(PackCmd),
}

#[derive(Subcommand, Debug)]
pub enum PackCmd {
    /// Import a signed skill pack (tarball + manifest files) as this
    /// tenant's pinned, read-only base layer. Works from an offline
    /// tarball — the air-gapped transport is the same call.
    Import {
        #[arg(long)]
        tenant: String,
        /// Input pack tarball file path.
        #[arg(long = "in")]
        input: String,
        /// Path to `pack.manifest.json` (defaults to
        /// `<in>.manifest.json`).
        #[arg(long)]
        manifest: Option<String>,
        /// Explicitly permit subscribing across verticals.
        #[arg(long)]
        allow_vertical_mismatch: bool,
    },
    /// Reviewed upgrade of a subscribed pack (the only operation that
    /// moves a version pin). Conflicts block unless acknowledged.
    Rebase {
        #[arg(long)]
        tenant: String,
        /// Input pack tarball (the NEW version).
        #[arg(long = "in")]
        input: String,
        /// Path to its manifest (defaults to `<in>.manifest.json`).
        #[arg(long)]
        manifest: Option<String>,
        /// Apply despite rebase_conflict Issues (the human review).
        #[arg(long)]
        acknowledge_conflicts: bool,
        /// Plan only: run the full validation + conflict scan on the
        /// server, apply nothing, report {would_import, would_remove}.
        #[arg(long)]
        dry_run: bool,
    },
    /// Verify a pack's signature + content hash LOCALLY — no server
    /// call. Reads the shared pack secret from `ESCUREL_PACK_SECRET`;
    /// the receiving half of the air-gapped transport, before anything
    /// is imported.
    Verify {
        /// Input pack tarball file path.
        #[arg(long = "in")]
        input: String,
        /// Path to its manifest (defaults to `<in>.manifest.json`).
        #[arg(long)]
        manifest: Option<String>,
    },
    /// Drop a pack subscription: removes its base pages + version pin.
    Unsubscribe {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        id: String,
    },
    /// The subscribed skill packs and their pinned versions.
    List {
        /// Kept for symmetry with the other pack commands; the gateway
        /// is single-tenant, so the value is not sent anywhere.
        #[arg(long)]
        tenant: String,
    },
    /// Propose a scrubbed pack candidate from this node's promotable
    /// skills (the L2→L3 harvest); writes the candidate tarball + its
    /// manifest to files for hub review. Default-deny + fail-closed;
    /// every submission is audit-evented server-side.
    SubmitPromotion {
        #[arg(long)]
        tenant: String,
        /// Candidate pack identity for hub review. (`--candidate-id`
        /// remains as a hidden back-compat alias.)
        #[arg(long, alias = "candidate-id")]
        id: String,
        /// The vertical the candidate belongs to.
        #[arg(long)]
        vertical: String,
        /// Promotable skill ids to harvest (repeatable).
        #[arg(long = "skill", required = true)]
        skills: Vec<String>,
        /// Output file path for the candidate tarball.
        #[arg(long)]
        out: String,
        /// Output file path for the manifest (defaults to
        /// `<out>.manifest.json`).
        #[arg(long)]
        manifest_out: Option<String>,
    },
    /// Build a versioned, HMAC-signed skill pack from a tenant's
    /// corpus; writes the tarball and its manifest to files.
    Export {
        #[arg(long)]
        tenant: String,
        /// Pack identity, e.g. `logistics-midmarket`.
        #[arg(long)]
        id: String,
        /// Monotonic pack version.
        #[arg(long)]
        version: u32,
        /// The vertical this pack belongs to.
        #[arg(long)]
        vertical: String,
        /// Publisher identity, e.g. `hub.stuttgart-ai`.
        #[arg(long)]
        publisher: String,
        /// Skill ids whose pages form the pack subtree (repeatable).
        #[arg(long = "skill", required = true)]
        skills: Vec<String>,
        /// Also bundle each skill's instance pages.
        #[arg(long)]
        include_instances: bool,
        /// Output file path for the pack tarball.
        #[arg(long)]
        out: String,
        /// Output file path for `pack.manifest.json` (defaults to
        /// `<out>.manifest.json`).
        #[arg(long)]
        manifest_out: Option<String>,
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
        /// New display name (optional — partial update, #247).
        #[arg(long)]
        name: Option<String>,
        /// Lifecycle status: `active` | `suspended` (#247).
        #[arg(long)]
        status: Option<String>,
        /// Embedding provider override: `zero` | `gemini` | `embeddinggemma`
        /// (#247). Changing it requires a `rebuild` to re-embed.
        #[arg(long)]
        embedding_provider: Option<String>,
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
        #[arg(long = "in")]
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

/// `pack verify`, purely LOCAL: check the manifest HMAC then the
/// tarball content hash with the shared secret from
/// `ESCUREL_PACK_SECRET`. Takes no client — main.rs dispatches it
/// BEFORE any transport is constructed, so a bogus `ESCUREL_SERVER` /
/// malformed `ESCUREL_TOKEN` cannot block an offline verification.
pub fn verify_pack_local(input: &str, manifest: Option<String>) -> Result<Value> {
    let secret = std::env::var("ESCUREL_PACK_SECRET").map_err(|_| {
        anyhow::anyhow!(
            "ESCUREL_PACK_SECRET is not set — `pack verify` checks the manifest \
             HMAC locally and needs the shared pack secret in the environment"
        )
    })?;
    let manifest_path = manifest.unwrap_or_else(|| format!("{input}.manifest.json"));
    let manifest: escurel_client::PackManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("reading manifest {manifest_path}"))?,
    )
    .with_context(|| format!("parsing manifest {manifest_path}"))?;
    let tarball = std::fs::read(input).with_context(|| format!("reading pack {input}"))?;
    escurel_types::pack::verify_pack(&manifest, &tarball, &secret)
        .map_err(|reason| anyhow::anyhow!(reason))?;
    Ok(json!({
        "ok": true,
        "pack": manifest.id,
        "version": manifest.version,
        "content_hash": manifest.content_hash,
    }))
}

fn tenant_json(s: Option<TenantSpec>) -> Value {
    match s {
        Some(s) => json!({
            "tenant_id": s.tenant_id,
            "display_name": opt(&s.display_name),
            "status": s.status,
            "quotas": s.quotas,
            "embedding_provider": s.embedding_provider,
        }),
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
            let p = client
                .rebuild(RebuildRequest {
                    tenant_id: tenant,
                    scope,
                })
                .await?;
            Ok(json!({ "done": p.done, "total": p.total }))
        }
        AdminCmd::CompactLanes { tenant } => {
            let p = client
                .compact_lanes(CompactLanesRequest { tenant_id: tenant })
                .await?;
            Ok(json!({
                "ops_compacted": p.ops_compacted,
                "bytes_reclaimed": p.bytes_reclaimed,
            }))
        }
        AdminCmd::Pack(PackCmd::Import {
            tenant,
            input,
            manifest,
            allow_vertical_mismatch,
        }) => {
            let manifest_path = manifest.unwrap_or_else(|| format!("{input}.manifest.json"));
            let manifest: escurel_client::PackManifest = serde_json::from_slice(
                &std::fs::read(&manifest_path)
                    .with_context(|| format!("reading manifest {manifest_path}"))?,
            )
            .with_context(|| format!("parsing manifest {manifest_path}"))?;
            let bytes = std::fs::read(&input).with_context(|| format!("reading pack {input}"))?;
            let r = client
                .import_pack(&tenant, &manifest, bytes, allow_vertical_mismatch)
                .await?;
            Ok(r)
        }
        AdminCmd::Pack(PackCmd::Rebase {
            tenant,
            input,
            manifest,
            acknowledge_conflicts,
            dry_run,
        }) => {
            let manifest_path = manifest.unwrap_or_else(|| format!("{input}.manifest.json"));
            let manifest: escurel_client::PackManifest = serde_json::from_slice(
                &std::fs::read(&manifest_path)
                    .with_context(|| format!("reading manifest {manifest_path}"))?,
            )
            .with_context(|| format!("parsing manifest {manifest_path}"))?;
            let bytes = std::fs::read(&input).with_context(|| format!("reading pack {input}"))?;
            let r = if dry_run {
                client
                    .rebase_pack_dry_run(&tenant, &manifest, bytes, acknowledge_conflicts)
                    .await
            } else {
                client
                    .rebase_pack(&tenant, &manifest, bytes, acknowledge_conflicts)
                    .await
            };
            r.map_err(Into::into)
        }
        // Normally intercepted in main.rs BEFORE any client exists (the
        // command is purely local); kept here so the dispatch stays total.
        AdminCmd::Pack(PackCmd::Verify { input, manifest }) => verify_pack_local(&input, manifest),
        AdminCmd::Pack(PackCmd::Unsubscribe { tenant, id }) => client
            .call_raw(
                "unsubscribe_pack",
                json!({ "tenant_id": tenant, "pack_id": id }),
            )
            .await
            .map_err(Into::into),
        AdminCmd::Pack(PackCmd::List { tenant }) => {
            let _ = tenant; // single-tenant gateway; kept for symmetry
            client.list_packs().await.map_err(Into::into)
        }
        AdminCmd::Pack(PackCmd::SubmitPromotion {
            tenant,
            id,
            vertical,
            skills,
            out,
            manifest_out,
        }) => {
            let (manifest, bytes, event_id) = client
                .submit_promotion(&tenant, &id, &vertical, &skills)
                .await?;
            let manifest_path = manifest_out.unwrap_or_else(|| format!("{out}.manifest.json"));
            let n = bytes.len();
            std::fs::write(&out, bytes).with_context(|| format!("writing candidate to {out}"))?;
            std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
                .with_context(|| format!("writing manifest to {manifest_path}"))?;
            Ok(json!({
                "bytes_exported": n,
                "path": out,
                "manifest_path": manifest_path,
                "event_id": event_id,
                "candidate": manifest.id,
            }))
        }
        AdminCmd::Pack(PackCmd::Export {
            tenant,
            id,
            version,
            vertical,
            publisher,
            skills,
            include_instances,
            out,
            manifest_out,
        }) => {
            let (manifest, bytes) = client
                .export_pack(ExportPackRequest {
                    tenant_id: tenant,
                    id,
                    version,
                    vertical,
                    publisher,
                    skills,
                    include_instances,
                })
                .await?;
            let manifest_path = manifest_out.unwrap_or_else(|| format!("{out}.manifest.json"));
            let n = bytes.len();
            std::fs::write(&out, bytes).with_context(|| format!("writing pack to {out}"))?;
            std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
                .with_context(|| format!("writing manifest to {manifest_path}"))?;
            Ok(json!({
                "bytes_exported": n,
                "path": out,
                "manifest_path": manifest_path,
                "content_hash": manifest.content_hash,
                "pack": format!("{}@v{}", manifest.id, manifest.version),
            }))
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
                        ..Default::default()
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
        TenantCmd::Update {
            id,
            name,
            status,
            embedding_provider,
        } => {
            // Get-modify-put (#247): fetch the current spec, apply only the
            // provided flags, write it back — so a partial update never
            // clobbers unspecified fields.
            let current = client
                .tenant_get(TenantGetRequest {
                    tenant_id: id.clone(),
                })
                .await?
                .spec
                .unwrap_or_else(|| TenantSpec {
                    tenant_id: id.clone(),
                    ..Default::default()
                });
            let mut spec = current;
            if let Some(n) = name {
                spec.display_name = n;
            }
            if let Some(s) = status {
                spec.status = match s.as_str() {
                    "suspended" => escurel_client::TenantStatus::Suspended,
                    _ => escurel_client::TenantStatus::Active,
                };
            }
            if let Some(p) = embedding_provider {
                spec.embedding_provider = Some(escurel_client::EmbeddingSpec {
                    provider: p,
                    ..Default::default()
                });
            }
            let r = client
                .tenant_update(TenantUpdateRequest { spec: Some(spec) })
                .await?;
            Ok(json!({ "tenant": tenant_json(r.spec), "rebuild_required": r.rebuild_required }))
        }
        TenantCmd::Delete { id } => {
            // The operator naming the tenant on the command line is the
            // confirmation the server's destructive-delete guard requires.
            let r = client
                .tenant_delete(TenantDeleteRequest {
                    confirm: Some(id.clone()),
                    tenant_id: id,
                })
                .await?;
            Ok(json!({ "deleted": r.deleted }))
        }
        TenantCmd::Export { id, out } => {
            let bytes = client
                .tenant_export(TenantExportRequest { tenant_id: id })
                .await?;
            let n = bytes.len();
            std::fs::write(&out, &bytes).with_context(|| format!("write export to {out}"))?;
            Ok(json!({ "bytes_exported": n, "path": out }))
        }
        TenantCmd::Import { id, input } => {
            let bytes =
                std::fs::read(&input).with_context(|| format!("read import from {input}"))?;
            let imported = client.tenant_import(&id, bytes).await?;
            Ok(json!({ "bytes_imported": imported }))
        }
    }
}
