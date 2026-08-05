//! Admin-role-gated tools: tenants, packs, credentials, endpoints, lanes,
//! groups, quota, audit, rebuild and snapshot operations.
//!
//! Split out of `mcp.rs` (R1 of `docs/notes/complexity-reduction-plan.md`).
//! This is the largest coherent group in the tool surface and the one least
//! entangled with the agent-facing read/write path: every tool here is gated
//! by `require_admin` in the dispatcher before it runs.

use super::*;

// --- admin ops tools (admin-role gated) ------------------------
//
// These mirror the documented MCP admin surface and delegate to the
// same logic the gRPC `EscurelAdmin` service uses. The role gate is
// applied by the dispatcher (`require_admin`) before these run.

pub(super) fn tool_admin_quota(
    state: &crate::server::AppState,
    tenant_id: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    // Honour the requested tenant: reject a `tenant_id` arg that names
    // a different tenant than this gateway serves, rather than silently
    // returning the caller's own snapshot.
    let req: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_quota: {e}")))?;
    if let Some(handle) = state.indexer.as_ref() {
        ensure_tenant_matches(&handle.current(), &req.tenant_id)?;
    }
    let quota = state
        .quota
        .as_ref()
        .ok_or_else(|| JsonRpcError::internal("no quota manager wired on this server"))?;
    let s = quota.snapshot(tenant_id);
    to_value(QuotaGetResponse {
        queries_remaining: s.queries_remaining,
        writes_remaining: s.writes_remaining,
        embeds_remaining: s.embeds_remaining,
        concurrent_sessions: s.concurrent_sessions_in_use,
    })
}

#[derive(Deserialize)]
pub(super) struct WebhookDeliveriesArgs {
    #[serde(default)]
    limit: Option<usize>,
}

/// Recent outbound-webhook delivery outcomes (newest first). Observability
/// for whether captures are reaching the agent runner. `configured: false`
/// when no `ESCUREL_WEBHOOK_URL` is set (nothing is ever sent).
pub(super) fn tool_admin_webhook_deliveries(
    state: &crate::server::AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: WebhookDeliveriesArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_webhook_deliveries: {e}")))?;
    let limit = a.limit.unwrap_or(100).min(200);
    let (configured, records) = match state.webhook.as_ref() {
        Some(w) => (true, w.recent(limit)),
        None => (false, Vec::new()),
    };
    to_value(WebhookDeliveriesResponse {
        configured,
        deliveries: records
            .into_iter()
            .map(|d| WebhookDelivery {
                event_id: d.event_id,
                at_ms: d.at_ms,
                ok: d.ok,
                http_status: d.http_status,
                error: d.error,
            })
            .collect(),
    })
}

pub(super) async fn tool_admin_audit(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let req: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_audit: {e}")))?;
    ensure_tenant_matches(indexer, &req.tenant_id)?;
    let drift = indexer
        .audit()
        .await
        .map_err(|e| JsonRpcError::internal(format!("admin_audit: {e}")))?;
    Ok(json!({
        "markdown_not_in_duckdb": drift.markdown_not_in_duckdb,
        "indexed_but_no_markdown": drift.indexed_but_no_markdown,
    }))
}

#[derive(Deserialize)]
pub(super) struct AdminIndexQueryArgs {
    table: String,
    #[serde(default = "default_inspect_limit")]
    limit: usize,
}

pub(super) fn default_inspect_limit() -> usize {
    100
}

pub(super) async fn tool_admin_index_query(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AdminIndexQueryArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_index_query: {e}")))?;
    let res = indexer
        .inspect_table(&a.table, a.limit)
        .await
        // Unknown-table / bad-arg errors are caller errors, not server
        // faults — surface as invalid_params.
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_index_query: {e}")))?;
    Ok(json!({
        "rows": res.rows,
        "schema": res.schema.iter().map(|c| json!({
            "name": c.name,
            "type": c.type_name,
        })).collect::<Vec<_>>(),
    }))
}

// --- admin lane introspection (mirrors EscurelAdmin gRPC) ---------

/// Canonical (and only) lane this server exposes.
pub(super) const LANE_NAME: &str = "markdown";
/// Hard cap on a single `admin_lane_blob` transfer (1 MiB).
pub(super) const LANE_BLOB_MAX_BYTES: u64 = 1024 * 1024;

pub(super) fn lane_name_ok(lane: &str) -> Result<(), JsonRpcError> {
    if lane.is_empty() || lane == LANE_NAME {
        Ok(())
    } else {
        Err(JsonRpcError::invalid_params(format!(
            "unknown lane `{lane}`; this server exposes only `{LANE_NAME}`"
        )))
    }
}

pub(super) fn lane_content_type(key: &str) -> &'static str {
    if key.ends_with(".md") {
        "text/markdown"
    } else if key.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

pub(super) fn tool_admin_list_lanes(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    Ok(json!({
        "lanes": [{
            "name": LANE_NAME,
            "backend": indexer.lane_store().backend(),
            "tenants_present": [indexer.tenant()],
        }],
    }))
}

#[derive(Deserialize)]
pub(super) struct AdminLaneKeysArgs {
    #[serde(default)]
    lane: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    limit: usize,
}

pub(super) async fn tool_admin_lane_keys(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AdminLaneKeysArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_keys: {e}")))?;
    lane_name_ok(&a.lane)?;
    let store = indexer.lane_store();
    let prefix = Key::new(indexer.tenant(), a.prefix)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_keys prefix: {e}")))?;
    let mut keys = store
        .list(&prefix)
        .await
        .map_err(|e| JsonRpcError::internal(format!("admin_lane_keys: {e}")))?;
    keys.sort_by(|x, y| x.path().cmp(y.path()));
    let limit = if a.limit == 0 { 100 } else { a.limit };
    let mut out = Vec::new();
    for k in keys.into_iter().take(limit) {
        let size = store
            .size(&k)
            .await
            .map_err(|e| JsonRpcError::internal(format!("admin_lane_keys size: {e}")))?;
        out.push(json!({ "key": k.path(), "size_bytes": size }));
    }
    Ok(json!({ "keys": out }))
}

#[derive(Deserialize)]
pub(super) struct AdminLaneBlobArgs {
    #[serde(default)]
    lane: String,
    key: String,
}

pub(super) async fn tool_admin_lane_blob(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AdminLaneBlobArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_blob: {e}")))?;
    lane_name_ok(&a.lane)?;
    let store = indexer.lane_store();
    let key = Key::new(indexer.tenant(), a.key.clone())
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_lane_blob key: {e}")))?;
    let size = store.size(&key).await.map_err(map_lane_err)?;
    if size > LANE_BLOB_MAX_BYTES {
        return Err(JsonRpcError::invalid_params(format!(
            "blob is {size} bytes, over the {LANE_BLOB_MAX_BYTES}-byte admin cap"
        )));
    }
    let bytes = store.read(&key).await.map_err(map_lane_err)?;
    to_value(AdminLaneBlobResponse {
        bytes_base64: B64.encode(&bytes),
        content_type: lane_content_type(&a.key).to_owned(),
    })
}

pub(super) fn map_lane_err(e: StoreError) -> JsonRpcError {
    match e {
        StoreError::NotFound(_) => JsonRpcError::invalid_params("lane key not found".to_owned()),
        other => JsonRpcError::internal(format!("lane: {other}")),
    }
}

#[derive(Deserialize)]
pub(super) struct AdminDeleteChatHistoryArgs {
    #[serde(default)]
    chat_group_id: Option<String>,
    #[serde(default)]
    before_ts: Option<String>,
    #[serde(default)]
    author: Option<String>,
}

pub(super) async fn tool_admin_delete_chat_history(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AdminDeleteChatHistoryArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("admin_delete_chat_history: {e}")))?;
    let deleted = indexer
        .delete_chat_history(
            a.chat_group_id.as_deref(),
            a.before_ts.as_deref(),
            a.author.as_deref(),
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("admin_delete_chat_history: {e}")))?;
    Ok(json!({ "deleted": deleted }))
}

#[derive(Deserialize)]
pub(super) struct GroupMemberArgs {
    group_id: String,
    subject: String,
}

#[derive(Deserialize)]
pub(super) struct ListGroupMembersArgs {
    group_id: String,
}

pub(super) async fn tool_add_group_member(
    indexer: &Indexer,
    added_by: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: GroupMemberArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("add_group_member: {e}")))?;
    indexer
        .add_group_member(&a.group_id, &a.subject, Some(added_by))
        .await
        .map_err(|e| JsonRpcError::internal(format!("add_group_member: {e}")))?;
    Ok(json!({ "ok": true }))
}

pub(super) async fn tool_remove_group_member(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: GroupMemberArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("remove_group_member: {e}")))?;
    indexer
        .remove_group_member(&a.group_id, &a.subject)
        .await
        .map_err(|e| JsonRpcError::internal(format!("remove_group_member: {e}")))?;
    Ok(json!({ "ok": true }))
}

pub(super) async fn tool_list_group_members(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListGroupMembersArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_group_members: {e}")))?;
    let members = indexer
        .list_group_members(&a.group_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_group_members: {e}")))?;
    let members: Vec<Value> = members
        .into_iter()
        .map(|m| {
            json!({
                "group_id": m.group_id,
                "subject": m.subject,
                "added_at": m.added_at,
                "added_by": m.added_by,
            })
        })
        .collect();
    Ok(json!({ "members": members }))
}

#[derive(Deserialize)]
pub(super) struct RegisterCredentialArgs {
    /// The `attach` name a `sql_view` skill references.
    name: String,
    /// Connector kind (`postgres`|`mysql`|`sqlite`|`erpl`|`s3`|…).
    connector: String,
    /// Secret material (DSN / secret spec). Stored server-side only.
    secret: String,
}

#[derive(Deserialize)]
pub(super) struct CredentialNameArgs {
    name: String,
}

pub(super) async fn tool_register_credential(
    indexer: &Indexer,
    created_by: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: RegisterCredentialArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("register_credential: {e}")))?;
    if a.name.is_empty() || a.connector.is_empty() || a.secret.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "name, connector, and secret are all required".to_owned(),
        ));
    }
    indexer
        .register_credential(&a.name, &a.connector, &a.secret, Some(created_by))
        .await
        .map_err(|e| JsonRpcError::internal(format!("register_credential: {e}")))?;
    // Never echo the secret back.
    Ok(json!({ "ok": true, "name": a.name }))
}

pub(super) async fn tool_list_credentials(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let creds = indexer
        .list_credentials()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_credentials: {e}")))?;
    // The secret is intentionally absent from this view (REQ-SQL-05).
    let creds: Vec<Value> = creds
        .into_iter()
        .map(|c| {
            json!({
                "name": c.name,
                "connector": c.connector,
                "created_at": c.created_at,
                "created_by": c.created_by,
            })
        })
        .collect();
    Ok(json!({ "credentials": creds }))
}

pub(super) async fn tool_delete_credential(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CredentialNameArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("delete_credential: {e}")))?;
    indexer
        .delete_credential(&a.name)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_credential: {e}")))?;
    Ok(json!({ "ok": true }))
}

pub(super) async fn tool_validate_bindings(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let statuses = indexer
        .validate_bindings()
        .await
        .map_err(|e| JsonRpcError::internal(format!("validate_bindings: {e}")))?;
    let degraded = statuses.iter().filter(|s| s.status != "ok").count();
    let bindings: Vec<Value> = statuses
        .into_iter()
        .map(|s| {
            json!({
                "page_id": s.page_id,
                "view": s.view,
                "status": s.status,
                "detail": s.detail,
            })
        })
        .collect();
    Ok(json!({ "ok": degraded == 0, "degraded": degraded, "bindings": bindings }))
}

#[derive(Deserialize)]
pub(super) struct CreateSqlInstanceArgs {
    skill: String,
    id: String,
    #[serde(default)]
    overlay_body: Option<String>,
}

/// Admin: materialise a sql_view instance from the UI. The binding comes from
/// the skill's `backend.source` block (not the caller), so this can only
/// create instances of skills that already declare a sql_view source.
pub(super) async fn tool_create_sql_instance(
    indexer: &std::sync::Arc<Indexer>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CreateSqlInstanceArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("create_sql_instance: {e}")))?;
    let binding = indexer
        .skill_backend(&a.skill)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_sql_instance: {e}")))?;
    let sql_view = binding.sql_view.ok_or_else(|| {
        JsonRpcError::invalid_params(format!(
            "skill `{}` does not declare a sql_view backend.source",
            a.skill
        ))
    })?;
    let body = a.overlay_body.unwrap_or_else(|| format!("# {}\n", a.id));
    let m = escurel_index::backend::SqlViewBackend::new(std::sync::Arc::clone(indexer))
        .create_instance(&a.skill, &sql_view, &a.id, &body)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_sql_instance: {e}")))?;
    Ok(json!({ "page_id": m.page_id, "view": m.view }))
}

// --- remote (openapi/mcp) backend tools ------------------------
//
// The endpoint registry (admin) holds each upstream's base URL + auth
// server-side (the SSRF / secrets-in-markdown guard); a skill's
// `backend.endpoint` references a row by name. `create_remote_instance`
// materialises an overlay page + `backend_ref`; `write_instance` forwards a
// write to the bound upstream, ACL-gated on the target.

#[derive(Deserialize)]
pub(super) struct RegisterEndpointArgs {
    name: String,
    /// `openapi` | `mcp`.
    kind: String,
    base_url: String,
    /// `none` (default) | `bearer` | `api_key`.
    #[serde(default)]
    auth: Option<String>,
    /// Header name when `auth = api_key` (default `X-API-Key`).
    #[serde(default)]
    auth_header: Option<String>,
    /// Bearer token / api-key material; stored server-side, never echoed.
    #[serde(default)]
    secret: Option<String>,
}

pub(super) async fn tool_register_endpoint(
    indexer: &Indexer,
    created_by: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: RegisterEndpointArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("register_endpoint: {e}")))?;
    if a.name.is_empty() || a.base_url.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "name and base_url are required".to_owned(),
        ));
    }
    if a.kind != "openapi" && a.kind != "mcp" {
        return Err(JsonRpcError::invalid_params(format!(
            "kind must be openapi|mcp, got `{}`",
            a.kind
        )));
    }
    let auth = match a.auth.as_deref().unwrap_or("none") {
        "none" => escurel_index::endpoints::EndpointAuth::None,
        "bearer" => escurel_index::endpoints::EndpointAuth::Bearer,
        "api_key" => escurel_index::endpoints::EndpointAuth::ApiKey {
            header: a
                .auth_header
                .clone()
                .unwrap_or_else(|| "X-API-Key".to_owned()),
        },
        other => {
            return Err(JsonRpcError::invalid_params(format!(
                "auth must be none|bearer|api_key, got `{other}`"
            )));
        }
    };
    let has_secret = a.secret.as_deref().is_some_and(|s| !s.is_empty());
    if !matches!(auth, escurel_index::endpoints::EndpointAuth::None) && !has_secret {
        return Err(JsonRpcError::invalid_params(
            "secret is required for bearer/api_key auth".to_owned(),
        ));
    }
    indexer
        .register_endpoint(
            &a.name,
            &a.kind,
            &a.base_url,
            &auth,
            a.secret.as_deref(),
            Some(created_by),
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("register_endpoint: {e}")))?;
    // Never echo the secret back.
    Ok(json!({ "ok": true, "name": a.name }))
}

pub(super) async fn tool_list_endpoints(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let eps = indexer
        .list_endpoints()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_endpoints: {e}")))?;
    // The secret is intentionally absent from this view (REQ-REMOTE-05).
    let eps: Vec<Value> = eps
        .into_iter()
        .map(|e| {
            json!({
                "name": e.name,
                "kind": e.kind,
                "base_url": e.base_url,
                "auth_scheme": e.auth_scheme,
                "created_at": e.created_at,
                "created_by": e.created_by,
            })
        })
        .collect();
    Ok(json!({ "endpoints": eps }))
}

pub(super) async fn tool_delete_endpoint(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    #[derive(Deserialize)]
    struct A {
        name: String,
    }
    let a: A = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("delete_endpoint: {e}")))?;
    indexer
        .delete_endpoint(&a.name)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_endpoint: {e}")))?;
    Ok(json!({ "ok": true }))
}

pub(super) async fn tool_validate_endpoints(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let eps = indexer
        .list_endpoints()
        .await
        .map_err(|e| JsonRpcError::internal(format!("validate_endpoints: {e}")))?;
    let mut out = Vec::new();
    let mut unreachable = 0usize;
    for e in eps {
        let rec = indexer
            .lookup_endpoint(&e.name)
            .await
            .map_err(|err| JsonRpcError::internal(format!("validate_endpoints: {err}")))?;
        let (status, detail) = match rec {
            Some(rec) => crate::remote_backend::probe(&rec).await,
            None => (
                "unreachable".to_owned(),
                Some("endpoint vanished".to_owned()),
            ),
        };
        if status != "ok" {
            unreachable += 1;
        }
        out.push(json!({
            "name": e.name, "kind": e.kind, "status": status, "detail": detail,
        }));
    }
    Ok(json!({ "ok": unreachable == 0, "unreachable": unreachable, "endpoints": out }))
}

#[derive(Deserialize)]
pub(super) struct CreateRemoteInstanceArgs {
    skill: String,
    id: String,
    #[serde(default)]
    overlay_body: Option<String>,
}

/// Admin: materialise a remote (openapi/mcp) overlay page from a skill that
/// declares a remote backend. The binding comes from the skill's `backend:`
/// block (not the caller), mirroring `create_sql_instance`, so this can only
/// create instances of skills that already declare a remote backend whose
/// endpoint is registered.
pub(super) async fn tool_create_remote_instance(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CreateRemoteInstanceArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("create_remote_instance: {e}")))?;
    let binding = indexer
        .skill_backend(&a.skill)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_remote_instance: {e}")))?;
    if !binding.kind.is_remote() {
        return Err(JsonRpcError::invalid_params(format!(
            "skill `{}` does not declare a remote (openapi/mcp) backend",
            a.skill
        )));
    }
    let remote = binding.remote.ok_or_else(|| {
        JsonRpcError::invalid_params(format!(
            "skill `{}` has an incomplete remote backend binding (endpoint/read missing)",
            a.skill
        ))
    })?;
    let kind = binding.kind.as_str();
    let endpoint = remote.endpoint.clone();
    // Fail closed when the referenced endpoint is not registered.
    if indexer
        .lookup_endpoint(&endpoint)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_remote_instance: {e}")))?
        .is_none()
    {
        return Err(JsonRpcError::invalid_params(format!(
            "endpoint `{endpoint}` is not registered"
        )));
    }
    let body = a.overlay_body.unwrap_or_else(|| format!("# {}\n", a.id));
    let page_id = format!("markdown/instances/{}/{}.md", a.skill, a.id);
    let content = format!(
        "---\n\
         type: instance\n\
         skill: {skill}\n\
         id: {id}\n\
         backend_ref:\n\
        \x20 kind: {kind}\n\
        \x20 endpoint: {endpoint}\n\
         ---\n\
         {body}\n",
        skill = a.skill,
        id = a.id,
    );
    indexer
        .update_page(&page_id, &content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_remote_instance: {e}")))?;
    Ok(json!({ "page_id": page_id, "kind": kind, "endpoint": endpoint }))
}

#[derive(Deserialize)]
pub(super) struct WriteInstanceArgs {
    /// The target instance id or its `[[skill::id]]` wikilink.
    #[serde(rename = "ref")]
    reference: String,
    /// The write payload forwarded to the upstream `write` op.
    #[serde(default)]
    payload: Value,
}

/// Write-back to a remote instance's upstream (openapi/mcp). Gated by the
/// target instance's `acl.update` (fail-closed; admin bypasses). A skill whose
/// binding declares no `write` op is refused.
pub(super) async fn tool_write_instance(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: WriteInstanceArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("write_instance: {e}")))?;
    let link = if a.reference.starts_with("[[") {
        a.reference.clone()
    } else {
        format!("[[{}]]", a.reference)
    };
    let page = indexer
        .resolve(&link, None)
        .await
        .map_err(|e| JsonRpcError::internal(format!("write_instance: {e}")))?
        .page
        .ok_or_else(|| {
            JsonRpcError::invalid_params(format!("no instance for ref `{}`", a.reference))
        })?;
    // Load the target's frontmatter (for the ACL decision) via expand.
    let expanded = indexer
        .expand(&page.page_id, None, None)
        .await
        .map_err(|e| JsonRpcError::internal(format!("write_instance: {e}")))?
        .ok_or_else(|| {
            JsonRpcError::invalid_params(format!("no instance for ref `{}`", a.reference))
        })?;
    // Gate on acl.update of the target instance (fail-closed; admin bypasses).
    let allowed = indexer
        .may_write_instance(
            &caller,
            &page.skill,
            Some(&expanded.frontmatter),
            &expanded.frontmatter,
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("write_instance acl: {e}")))?;
    if !allowed {
        return Err(JsonRpcError::invalid_params(
            "not authorised to write this instance".to_owned(),
        ));
    }
    crate::remote_backend::write_instance(indexer, &page.skill, page.slug.as_deref(), &a.payload)
        .await
        .map_err(|e| JsonRpcError::internal(format!("write_instance: {e}")))
}

// --- admin tenant CRUD + long-ops (admin-role gated) -----------
//
// These port the gRPC `EscurelAdmin` business logic verbatim; only
// the transport wrapper changes. The role gate is applied by the
// dispatcher (`require_admin`) before these run. gRPC error codes
// (not_found / invalid_argument / failed_precondition) map onto the
// JSON-RPC `internal` / `invalid_params` envelope with a clear
// message.

/// `state.tenant_store` or a failed-precondition error mirroring the
/// gRPC `tenant_store()` accessor.
pub(super) fn tenant_store(state: &AppState) -> Result<&Arc<dyn TenantStore>, JsonRpcError> {
    state
        .tenant_store
        .as_ref()
        .ok_or_else(|| JsonRpcError::internal("server has no tenant_store wired"))
}

/// The CURRENT indexer from `state.indexer` (captured once per admin
/// tool call — hot-swap seam) or a failed-precondition error.
pub(super) fn admin_indexer(state: &AppState) -> Result<Arc<Indexer>, JsonRpcError> {
    state
        .indexer
        .as_ref()
        .map(IndexerHandle::current)
        .ok_or_else(|| JsonRpcError::internal("server has no indexer wired"))
}

/// Reject an admin tool whose `tenant_id` argument targets a tenant
/// other than the one this single-tenant gateway is bound to. An empty
/// arg means "this gateway's tenant" and always passes. Without this
/// guard a `--tenant other` request silently operates on / reports the
/// wrong tenant (the gRPC admin surface enforced the same match).
pub(super) fn ensure_tenant_matches(
    indexer: &Indexer,
    tenant_id: &str,
) -> Result<(), JsonRpcError> {
    if !tenant_id.is_empty() && tenant_id != indexer.tenant() {
        return Err(JsonRpcError::failed_precondition(format!(
            "tenant `{tenant_id}` does not match this gateway's tenant `{}`",
            indexer.tenant()
        )));
    }
    Ok(())
}

/// Map an `AdminError` onto the JSON-RPC envelope, mirroring the
/// gRPC status mapping: invalid id → invalid_params; everything else
/// (already-exists, I/O, duckdb) → internal.
pub(super) fn map_admin_err(e: escurel_admin::AdminError) -> JsonRpcError {
    match e {
        escurel_admin::AdminError::InvalidTenantId(_) => {
            JsonRpcError::invalid_params(e.to_string())
        }
        other => JsonRpcError::internal(other.to_string()),
    }
}

#[derive(Deserialize)]
pub(super) struct TenantSpecArgs {
    #[serde(default)]
    tenant_id: String,
    /// Optional so `tenant_update` is a **partial** update (#247): a field
    /// omitted from the request keeps its current value.
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    status: Option<escurel_types::TenantStatus>,
    #[serde(default)]
    quotas: Option<escurel_types::QuotaOverride>,
    #[serde(default)]
    embedding_provider: Option<escurel_types::EmbeddingSpec>,
}

/// Convert the persisted admin spec into the wire spec. The lifecycle/quota/
/// embedding sub-types are shared from `escurel-types`, so this is a plain move.
pub(super) fn admin_to_wire(s: AdminTenantSpec) -> TypesTenantSpec {
    TypesTenantSpec {
        tenant_id: s.tenant_id,
        display_name: s.display_name,
        status: s.status,
        quotas: s.quotas,
        embedding_provider: s.embedding_provider,
    }
}

#[derive(Deserialize)]
pub(super) struct TenantIdArgs {
    #[serde(default)]
    tenant_id: String,
}

pub(super) async fn tool_tenant_create(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: TenantSpecArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_create: {e}")))?;
    let store = tenant_store(state)?.clone();
    let spec = AdminTenantSpec {
        tenant_id: a.tenant_id,
        display_name: a.display_name.unwrap_or_default(),
        status: a.status.unwrap_or_default(),
        quotas: a.quotas,
        embedding_provider: a.embedding_provider,
    };
    store.create(&spec).await.map_err(map_admin_err)?;
    to_value(TenantCreateResponse {
        spec: Some(admin_to_wire(spec)),
    })
}

pub(super) async fn tool_tenant_list(state: &AppState) -> Result<Value, JsonRpcError> {
    let store = tenant_store(state)?.clone();
    let specs = store.list().await.map_err(map_admin_err)?;
    to_value(TenantListResponse {
        tenants: specs.into_iter().map(admin_to_wire).collect(),
    })
}

pub(super) async fn tool_tenant_get(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_get: {e}")))?;
    let store = tenant_store(state)?.clone();
    match store.get(&a.tenant_id).await.map_err(map_admin_err)? {
        None => Err(JsonRpcError::invalid_params(format!(
            "tenant `{}` not found",
            a.tenant_id
        ))),
        Some(spec) => to_value(TenantGetResponse {
            spec: Some(admin_to_wire(spec)),
        }),
    }
}

pub(super) async fn tool_tenant_update(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: TenantSpecArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_update: {e}")))?;
    let store = tenant_store(state)?.clone();
    // Partial update (#247): read the current spec, overlay the provided
    // fields, write it back.
    let mut spec = match store.get(&a.tenant_id).await.map_err(map_admin_err)? {
        Some(s) => s,
        None => {
            return Err(JsonRpcError::invalid_params(format!(
                "tenant `{}` not found",
                a.tenant_id
            )));
        }
    };
    if let Some(dn) = a.display_name {
        spec.display_name = dn;
    }
    if let Some(st) = a.status {
        spec.status = st;
    }
    if let Some(q) = a.quotas {
        spec.quotas = Some(q);
    }
    let embedding_changed =
        a.embedding_provider.is_some() && a.embedding_provider != spec.embedding_provider;
    if let Some(ep) = a.embedding_provider {
        spec.embedding_provider = Some(ep);
    }
    store.update(&spec).await.map_err(map_admin_err)?;

    // Live side effects apply only to the SERVED tenant (single-tenant-per-
    // process); other tenants pick the new spec up at their next boot. The
    // embedding provider moves the vector space, so it only takes effect on
    // the next boot/rebuild — hence `rebuild_required`, not a live swap.
    if state.served_tenant.as_deref() == Some(spec.tenant_id.as_str()) {
        state.tenant_suspended.store(
            matches!(spec.status, escurel_types::TenantStatus::Suspended),
            std::sync::atomic::Ordering::Relaxed,
        );
        if let (Some(q), Some(over)) = (state.quota.as_ref(), spec.quotas) {
            q.set_for_tenant(
                &spec.tenant_id,
                crate::config::apply_quota_override(escurel_quota::QuotaConfig::defaults(), over),
            );
        }
    }
    to_value(TenantUpdateResponse {
        spec: Some(admin_to_wire(spec)),
        rebuild_required: embedding_changed,
    })
}

#[derive(Deserialize)]
pub(super) struct TenantDeleteArgs {
    tenant_id: String,
    /// Confirmation token — must equal `tenant_id` for the destructive delete
    /// to proceed (protocol.md §Admin surface, platform.md §Tenant lifecycle).
    #[serde(default)]
    confirm: Option<String>,
}

pub(super) async fn tool_tenant_delete(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: TenantDeleteArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_delete: {e}")))?;
    // Fail closed on the destructive wipe unless the caller echoes the tenant
    // id back as `confirm` — guards against a fat-fingered tenant_id.
    if a.confirm.as_deref() != Some(a.tenant_id.as_str()) {
        return Err(JsonRpcError::invalid_params(format!(
            "tenant_delete requires confirm = \"{}\" (the tenant id) to proceed",
            a.tenant_id
        )));
    }
    let store = tenant_store(state)?.clone();
    let deleted = store.delete(&a.tenant_id).await.map_err(map_admin_err)?;
    to_value(TenantDeleteResponse { deleted })
}

#[derive(Deserialize)]
pub(super) struct ExportPackArgs {
    tenant_id: String,
    id: String,
    version: u32,
    vertical: String,
    publisher: String,
    skills: Vec<String>,
    #[serde(default)]
    include_instances: bool,
}

/// Admin: build a versioned, HMAC-signed skill pack (REQ-PACK-01/02/04)
/// from this tenant's corpus — the L3→L2 unit of distribution. Fails
/// closed when no `ESCUREL_PACK_SECRET` is configured (packs are
/// signed, always) and when any selected page trips the deterministic
/// secret scrub (INV-SECRETFREE).
pub(super) async fn tool_export_pack(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: ExportPackArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("export_pack: {e}")))?;
    let Some(secret) = state.pack_secret.clone() else {
        return Err(JsonRpcError::internal(
            "pack_secret_not_configured: export_pack refuses to build an unsigned \
             pack; set ESCUREL_PACK_SECRET",
        ));
    };
    if a.id.is_empty() || a.vertical.is_empty() || a.publisher.is_empty() || a.skills.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "export_pack: id, vertical, publisher and at least one skill are required",
        ));
    }
    // Same token rules the importer enforces (defence in depth): the id
    // becomes the spoke's landing prefix + layer stamp.
    if !crate::pack::is_safe_pack_token(&a.id) || !crate::pack::is_safe_pack_token(&a.vertical) {
        return Err(JsonRpcError::invalid_params(
            "export_pack: id and vertical must be lowercase alphanumerics plus . _ - \
             (max 64 chars)",
        ));
    }
    let indexer = admin_indexer(state)?;
    // A wrong `tenant_id` must not silently export this gateway's
    // (only) tenant (mirrors `rebuild`).
    ensure_tenant_matches(&indexer, &a.tenant_id)?;

    let pages = indexer
        .collect_pack_pages(&a.skills, a.include_instances)
        .await
        .map_err(|e| match e {
            IndexerError::PackSkillMissing { .. } => JsonRpcError::invalid_params(e.to_string()),
            other => JsonRpcError::internal(format!("export_pack: {other}")),
        })?;

    // Fail-closed content hygiene BEFORE anything is bundled: one
    // credential-shaped page aborts the whole export.
    for (path, content) in &pages {
        if let Some(reason) = escurel_index::pack::pack_scrub_rejection(path, content) {
            return Err(JsonRpcError::internal(reason));
        }
    }

    // Tar + gzip on a blocking thread — the same discipline as
    // `tenant_export` (codex review: a large pack would otherwise tie
    // up a Tokio worker).
    let page_count = pages.len() as u32;
    let tarball = tokio::task::spawn_blocking(move || crate::pack::build_tarball(&pages))
        .await
        .map_err(|e| JsonRpcError::internal(format!("export_pack join error: {e}")))?
        .map_err(|e| JsonRpcError::internal(format!("export_pack tar: {e}")))?;
    let mut manifest = escurel_types::PackManifest {
        format_version: crate::pack::PACK_FORMAT_VERSION,
        id: a.id,
        version: a.version,
        vertical: a.vertical,
        publisher: a.publisher,
        page_count,
        content_hash: crate::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = crate::pack::sign_manifest(&manifest, &secret);

    let bytes = tarball.len() as u64;
    Ok(json!({
        "manifest": manifest,
        "tarball_b64": B64.encode(&tarball),
        "bytes": bytes,
    }))
}

#[derive(Deserialize)]
pub(super) struct ImportPackArgs {
    tenant_id: String,
    manifest: escurel_types::PackManifest,
    tarball_b64: String,
    /// Loud escape hatch for REQ-SUB-03: a cross-vertical subscription
    /// is refused unless the operator explicitly overrides.
    #[serde(default)]
    allow_vertical_mismatch: bool,
}

/// Admin: import a signed skill pack as this tenant's pinned, read-only
/// **base layer** (REQ-SUB-01/02/03) — the L3→L2 coupler. Fail-closed:
/// signature + content hash verify before anything is unpacked
/// (`pack_signature_invalid`); unsafe entry paths refuse
/// (`pack_malformed`); a version change on a subscribed pack refuses
/// (`pack_version_pinned` — upgrades are an explicit future `rebase`,
/// never silent); an unrelated vertical refuses (`vertical_mismatch`)
/// unless explicitly overridden. Transport-neutral by construction: the
/// caller supplies the bytes, so an air-gapped tarball import and a
/// live pull are the same call (INV-AIRGAP).
pub(super) async fn tool_import_pack(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: ImportPackArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("import_pack: {e}")))?;
    let Some(secret) = state.pack_secret.clone() else {
        return Err(JsonRpcError::internal(
            "pack_secret_not_configured: import_pack cannot verify a pack without \
             ESCUREL_PACK_SECRET",
        ));
    };
    // Identity tokens are interpolated into the landing page-id prefix
    // and the stamped `layer:` line — unsafe characters would smuggle
    // path segments or YAML keys, signed or not (agy review).
    if !crate::pack::is_safe_pack_token(&a.manifest.id) {
        return Err(JsonRpcError::internal(format!(
            "pack_id_invalid: `{}` is not a safe pack id (lowercase alphanumerics \
             plus . _ -, max 64 chars)",
            a.manifest.id.escape_default()
        )));
    }
    if !crate::pack::is_safe_pack_token(&a.manifest.vertical) {
        return Err(JsonRpcError::internal(format!(
            "pack_id_invalid: vertical `{}` is not a safe token",
            a.manifest.vertical.escape_default()
        )));
    }
    // Version 0 is the promotion-candidate sentinel (signed with the
    // same shared secret): importing one would bypass the hub curator's
    // maker/checker gate and squat the pack id against the approved v1
    // (codex/agy review). Published packs start at v1.
    if a.manifest.version == 0 {
        return Err(JsonRpcError::internal(format!(
            "pack_candidate_not_importable: `{}` is a promotion candidate \
             (version 0), not a published pack; a hub curator reviews and \
             publishes it under a real version first",
            a.manifest.id
        )));
    }
    let indexer = admin_indexer(state)?;
    ensure_tenant_matches(&indexer, &a.tenant_id)?;

    // 1. Trust before touch (REQ-PACK-02): authenticate the manifest,
    //    then bind the bytes to it — nothing is unpacked before this.
    let tarball = B64
        .decode(a.tarball_b64.as_bytes())
        .map_err(|e| JsonRpcError::invalid_params(format!("tarball_b64 is not base64: {e}")))?;
    crate::pack::verify_pack(&a.manifest, &tarball, &secret).map_err(JsonRpcError::internal)?;

    // 2. Subscription pins. Same pack id: only the pinned version may
    //    re-import (idempotent refresh); anything else is an explicit
    //    future rebase. New pack id: the vertical guard applies.
    let subs = indexer
        .list_pack_subscriptions()
        .await
        .map_err(|e| JsonRpcError::internal(format!("import_pack subscriptions: {e}")))?;
    if let Some(existing) = subs.iter().find(|s| s.pack_id == a.manifest.id) {
        if existing.version != a.manifest.version {
            return Err(JsonRpcError::internal(format!(
                "pack_version_pinned: pack `{}` is pinned at v{}; importing v{} requires \
                 an explicit rebase (upgrades never happen silently)",
                a.manifest.id, existing.version, a.manifest.version
            )));
        }
        // Same version, different bytes: a re-published v{N} would let a
        // hub mutate a pinned base without the version moving (codex
        // review). Idempotent re-import means the SAME content.
        if existing.content_hash != a.manifest.content_hash {
            return Err(JsonRpcError::internal(format!(
                "pack_content_mismatch: pack `{}`@v{} is pinned at {} but this bundle \
                 hashes to {}; same-version re-publishes are refused — bump the pack \
                 version instead",
                a.manifest.id, a.manifest.version, existing.content_hash, a.manifest.content_hash
            )));
        }
    } else if let Some(other) = subs.iter().find(|s| s.vertical != a.manifest.vertical)
        && !a.allow_vertical_mismatch
    {
        return Err(JsonRpcError::internal(format!(
            "vertical_mismatch: this node is subscribed to vertical `{}` (pack `{}`) \
             but `{}` declares vertical `{}`; cross-vertical mixing resets the \
             convergence ramp — pass allow_vertical_mismatch=true to override",
            other.vertical, other.pack_id, a.manifest.id, a.manifest.vertical
        )));
    }

    // 3. Unpack, then validate + stamp EVERY entry before the first
    //    write (codex review / agy MUST-FIX 5): a malformed page means
    //    zero landed pages, never a half-imported base layer. Path
    //    safety (zip-slip) is enforced inside `unpack_entries`.
    let entries = crate::pack::unpack_entries(&tarball).map_err(JsonRpcError::internal)?;
    let layer = format!("base@{}@v{}", a.manifest.id, a.manifest.version);
    let prefix = format!(
        "{}{}/",
        escurel_index::pack::RESERVED_BASE_PREFIX,
        a.manifest.id
    );
    let mut stamped_pages: Vec<(String, String)> = Vec::with_capacity(entries.len());
    for (rel, content) in &entries {
        let stamped = crate::pack::stamp_layer(content, &layer).map_err(JsonRpcError::internal)?;
        stamped_pages.push((format!("{prefix}{rel}"), stamped));
    }

    // A skill page whose id another skill page already declares — an
    // indexed one OR another entry of this same pack (codex review: the
    // DB knows nothing about pages that haven't landed) — would make
    // slug resolution non-deterministic (silent shadowing). Checked
    // BEFORE the first write; re-imports of the same pack land on the
    // same page ids and pass.
    let mut skill_ids_in_pack: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (page_id, stamped) in &stamped_pages {
        let Ok(parsed) = escurel_md::parse(stamped) else {
            continue; // stamp_layer already parsed; defensive only
        };
        if parsed.frontmatter.page_type != PageType::Skill {
            continue;
        }
        let skill_id = parsed
            .frontmatter
            .fields
            .get("id")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or_default()
            .to_owned();
        if skill_id.is_empty() {
            continue;
        }
        if !skill_ids_in_pack.insert(skill_id.clone()) {
            return Err(JsonRpcError::internal(format!(
                "pack_skill_collision: pack `{}` ships skill `{skill_id}` more than \
                 once; two pages declaring one skill id resolve non-deterministically",
                a.manifest.id
            )));
        }
        if let Some(existing) = indexer
            .skill_page_conflict(&skill_id, page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("import_pack conflict check: {e}")))?
        {
            // A colliding TENANT OVERLAY page is the shadow feature
            // (REQ-LAYER-03): the overlay wins for display, the base
            // lands beneath it. Only a collision with ANOTHER pack's
            // base page refuses — no precedence exists between two
            // base pages.
            if existing.starts_with(escurel_index::pack::RESERVED_BASE_PREFIX) {
                return Err(JsonRpcError::internal(format!(
                    "pack_skill_collision: pack `{}` ships skill `{skill_id}` but \
                     another pack already provides it at `{existing}`; two base \
                     pages declaring one skill id resolve non-deterministically — \
                     unsubscribe the other pack first",
                    a.manifest.id
                )));
            }
        }
    }

    // All validation done — land the pages (lane store + index in one
    // step; upsert ⇒ idempotent). A mid-loop failure here is an I/O
    // catastrophe (disk full), not a content problem; the pin below is
    // still only written after every page landed.
    let mut pages_imported = 0u32;
    for (page_id, stamped) in &stamped_pages {
        indexer
            .update_page(page_id, stamped)
            .await
            .map_err(|e| JsonRpcError::internal(format!("import_pack `{page_id}`: {e}")))?;
        pages_imported += 1;
    }
    // FTS has no incremental refresh; rebuild it over the now-landed
    // blocks so the imported pages are searchable (same discipline as
    // `seed_from_dir`).
    indexer
        .refresh_fts()
        .await
        .map_err(|e| JsonRpcError::internal(format!("import_pack refresh_fts: {e}")))?;

    // 4. Record the pin LAST — a failed import must not leave a
    //    subscription row claiming pages that never landed.
    indexer
        .record_pack_subscription(&escurel_index::pack::PackSubscription {
            pack_id: a.manifest.id.clone(),
            version: a.manifest.version,
            vertical: a.manifest.vertical.clone(),
            publisher: a.manifest.publisher.clone(),
            content_hash: a.manifest.content_hash.clone(),
            signature: a.manifest.signature.clone(),
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("import_pack subscription: {e}")))?;

    Ok(json!({
        "pack": a.manifest.id,
        "version": a.manifest.version,
        "vertical": a.manifest.vertical,
        "pages_imported": pages_imported,
        "layer": layer,
    }))
}

#[derive(Deserialize)]
pub(super) struct SubmitPromotionArgs {
    tenant_id: String,
    /// The candidate pack identity the hub curator will review under.
    candidate_id: String,
    vertical: String,
    skills: Vec<String>,
}

/// Admin ("curator" in the v1 two-role model): propose a scrubbed pack
/// candidate from this node's own curated skills — the L2→L3 harvest
/// coupler and THE security-critical federation seam (REQ-PROMO-01..04).
/// Fail-closed, default-deny: only tenant-authored SKILL pages carrying
/// the curator-set `promotable: true` marker are eligible (raw instance
/// data never promotes; base-layer pages are the hub's, not ours); one
/// ineligible id or one credential-shaped page refuses the WHOLE
/// submission. Maker/checker: this tool *proposes* — a human curator at
/// the hub reviews the candidate and publishes it deliberately
/// (`export_pack` on the hub side); nothing auto-publishes. Every
/// submission emits an immutable audit event recording what left, when,
/// submitted by whom.
pub(super) async fn tool_submit_promotion(
    state: &AppState,
    subject: &str,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: SubmitPromotionArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("submit_promotion: {e}")))?;
    let Some(secret) = state.pack_secret.clone() else {
        return Err(JsonRpcError::internal(
            "pack_secret_not_configured: submit_promotion signs its candidate; set \
             ESCUREL_PACK_SECRET",
        ));
    };
    if a.skills.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "submit_promotion: at least one skill is required",
        ));
    }
    if !crate::pack::is_safe_pack_token(&a.candidate_id)
        || !crate::pack::is_safe_pack_token(&a.vertical)
    {
        return Err(JsonRpcError::internal(
            "pack_id_invalid: candidate_id and vertical must be lowercase \
             alphanumerics plus . _ - (max 64 chars)",
        ));
    }
    let indexer = admin_indexer(state)?;
    ensure_tenant_matches(&indexer, &a.tenant_id)?;

    // Default-deny eligibility (skills-only, promotable, overlay).
    let pages = indexer
        .collect_promotion_pages(&a.skills)
        .await
        .map_err(|e| match e {
            IndexerError::PromotionNotEligible { .. } => JsonRpcError::internal(e.to_string()),
            other => JsonRpcError::internal(format!("submit_promotion: {other}")),
        })?;

    // The deterministic scrubber — the same deny set the export path
    // runs (INV-SECRETFREE); one hit aborts the whole submission.
    for (path, content) in &pages {
        if let Some(reason) = escurel_index::pack::pack_scrub_rejection(path, content) {
            return Err(JsonRpcError::internal(reason));
        }
    }

    let page_paths: Vec<&str> = pages.iter().map(|(p, _)| p.as_str()).collect();
    let body_summary = serde_json::to_string(&json!({
        "candidate": a.candidate_id,
        "vertical": a.vertical,
        "pages": page_paths,
    }))
    .unwrap_or_default();

    let page_count = pages.len() as u32;
    let tarball = tokio::task::spawn_blocking(move || crate::pack::build_tarball(&pages))
        .await
        .map_err(|e| JsonRpcError::internal(format!("submit_promotion join error: {e}")))?
        .map_err(|e| JsonRpcError::internal(format!("submit_promotion tar: {e}")))?;
    let mut manifest = escurel_types::PackManifest {
        format_version: crate::pack::PACK_FORMAT_VERSION,
        id: a.candidate_id.clone(),
        // A candidate is not a published version; the hub assigns the
        // real version when a curator approves and publishes.
        version: 0,
        vertical: a.vertical.clone(),
        publisher: format!("spoke.{}", indexer.tenant()),
        page_count,
        content_hash: crate::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = crate::pack::sign_manifest(&manifest, &secret);

    // The immutable audit record (REQ-PROMO-04): what left this node,
    // when, submitted by whom — replayable, contract-grade.
    let event = indexer
        .capture_event(escurel_index::NewEvent {
            event_id: None,
            at: None,
            source: "promotion".to_owned(),
            mime: "application/json".to_owned(),
            label_skill: String::new(),
            instance_page_id: None,
            title: format!(
                "promotion.submitted: {} ({} page(s))",
                a.candidate_id, page_count
            ),
            body: body_summary,
            provenance: Some(json!({
                "submitted_by": subject,
                "content_hash": manifest.content_hash,
                "vertical": a.vertical,
            })),
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("submit_promotion audit event: {e}")))?;

    let bytes = tarball.len() as u64;
    Ok(json!({
        "manifest": manifest,
        "tarball_b64": B64.encode(&tarball),
        "bytes": bytes,
        "event_id": event.event_id,
    }))
}

#[derive(Deserialize)]
pub(super) struct RebasePackArgs {
    tenant_id: String,
    manifest: escurel_types::PackManifest,
    tarball_b64: String,
    /// The human half of the review: conflicts block until the operator
    /// explicitly acknowledges them.
    #[serde(default)]
    acknowledge_conflicts: bool,
    /// Plan only: run the full validation + conflict scan, apply
    /// NOTHING, and report what a real rebase would do.
    #[serde(default)]
    dry_run: bool,
}

/// Admin: the reviewed upgrade of a subscribed pack (REQ-REBASE-01/02)
/// — the ONLY operation that moves a version pin. Conflicts — the
/// tenant's shadow overrides a field the new version also changed —
/// surface as typed `rebase_conflict` Issues and block until the
/// operator passes `acknowledge_conflicts`; nothing auto-resolves.
/// Trust/validation mirrors `import_pack` (verify before unpack, whole
/// pack validates before the first write); orphaned base pages the new
/// version no longer ships are removed; the pin moves LAST.
pub(super) async fn tool_rebase_pack(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: RebasePackArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("rebase_pack: {e}")))?;
    let Some(secret) = state.pack_secret.clone() else {
        return Err(JsonRpcError::internal(
            "pack_secret_not_configured: rebase_pack cannot verify a pack without \
             ESCUREL_PACK_SECRET",
        ));
    };
    if !crate::pack::is_safe_pack_token(&a.manifest.id)
        || !crate::pack::is_safe_pack_token(&a.manifest.vertical)
    {
        return Err(JsonRpcError::internal(
            "pack_id_invalid: id and vertical must be safe tokens",
        ));
    }
    if a.manifest.version == 0 {
        return Err(JsonRpcError::internal(
            "pack_candidate_not_importable: version 0 is the promotion-candidate \
             sentinel; a rebase target is a published version",
        ));
    }
    let indexer = admin_indexer(state)?;
    ensure_tenant_matches(&indexer, &a.tenant_id)?;

    let tarball = B64
        .decode(a.tarball_b64.as_bytes())
        .map_err(|e| JsonRpcError::invalid_params(format!("tarball_b64 is not base64: {e}")))?;
    crate::pack::verify_pack(&a.manifest, &tarball, &secret).map_err(JsonRpcError::internal)?;

    let subs = indexer
        .list_pack_subscriptions()
        .await
        .map_err(|e| JsonRpcError::internal(format!("rebase_pack subscriptions: {e}")))?;
    let Some(existing) = subs.iter().find(|s| s.pack_id == a.manifest.id) else {
        return Err(JsonRpcError::internal(format!(
            "pack_not_subscribed: `{}` has no subscription on this node — use \
             import_pack for a first subscription",
            a.manifest.id
        )));
    };
    if a.manifest.version <= existing.version {
        return Err(JsonRpcError::internal(format!(
            "pack_rebase_not_an_upgrade: `{}` is pinned at v{}; a rebase target must \
             be a later version (same-version refreshes are import_pack's job)",
            a.manifest.id, existing.version
        )));
    }
    let from_version = existing.version;

    // Validate + stamp the WHOLE incoming version before any write —
    // the same discipline as import.
    let entries = crate::pack::unpack_entries(&tarball).map_err(JsonRpcError::internal)?;
    let layer = format!("base@{}@v{}", a.manifest.id, a.manifest.version);
    let prefix = format!(
        "{}{}/",
        escurel_index::pack::RESERVED_BASE_PREFIX,
        a.manifest.id
    );
    let mut stamped_pages: Vec<(String, String)> = Vec::with_capacity(entries.len());
    for (rel, content) in &entries {
        let stamped = crate::pack::stamp_layer(content, &layer).map_err(JsonRpcError::internal)?;
        stamped_pages.push((format!("{prefix}{rel}"), stamped));
    }
    let mut skill_ids_in_pack: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (page_id, stamped) in &stamped_pages {
        let Ok(parsed) = escurel_md::parse(stamped) else {
            continue;
        };
        if parsed.frontmatter.page_type != PageType::Skill {
            continue;
        }
        let skill_id = parsed
            .frontmatter
            .fields
            .get("id")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or_default()
            .to_owned();
        if skill_id.is_empty() {
            continue;
        }
        if !skill_ids_in_pack.insert(skill_id.clone()) {
            return Err(JsonRpcError::internal(format!(
                "pack_skill_collision: `{}` v{} ships skill `{skill_id}` more than once",
                a.manifest.id, a.manifest.version
            )));
        }
        if let Some(other) = indexer
            .skill_page_conflict(&skill_id, page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("rebase_pack conflict check: {e}")))?
            && !other.starts_with(&prefix)
        {
            return Err(JsonRpcError::internal(format!(
                "pack_skill_collision: `{}` v{} ships skill `{skill_id}` but another \
                 pack provides it at `{other}`",
                a.manifest.id, a.manifest.version
            )));
        }
    }

    // Conflict detection (REQ-REBASE-01): for every incoming page whose
    // OLD base a tenant overlay shadows, a field the upstream changed
    // AND the overlay overrides is a conflict the operator must see.
    // "Field" includes the body. Deterministic set intersection — no
    // merge, no heuristics.
    let mut issues: Vec<Value> = Vec::new();
    for (_page_id, stamped) in &stamped_pages {
        let Ok(new_page) = escurel_md::parse(stamped) else {
            continue;
        };
        if new_page.frontmatter.page_type != PageType::Skill {
            continue;
        }
        let skill_id = new_page
            .frontmatter
            .fields
            .get("id")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or_default()
            .to_owned();
        // The shadow, if any: the overlay skill page found BY SLUG —
        // a shadow can live at any page id, not only the canonical
        // `markdown/skills/<id>.md` (codex review).
        let Some(overlay_id) = indexer
            .overlay_skill_page_id(&skill_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("rebase_pack overlay lookup: {e}")))?
        else {
            continue;
        };
        let Some(overlay_content) = indexer
            .page_content(&overlay_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("rebase_pack overlay read: {e}")))?
        else {
            continue;
        };
        let Ok(overlay) = escurel_md::parse(&overlay_content) else {
            continue;
        };
        // The currently-pinned base page — found BY SLUG within this
        // pack's namespace, so an upstream file move cannot dodge the
        // diff (codex review). Absent for skills new in vN+1.
        let Some(old_base_id) = indexer
            .pack_base_skill_page_id(&skill_id, &a.manifest.id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("rebase_pack base lookup: {e}")))?
        else {
            continue;
        };
        let Some(old_content) = indexer
            .page_content(&old_base_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("rebase_pack base read: {e}")))?
        else {
            continue;
        };
        let Ok(old_page) = escurel_md::parse(&old_content) else {
            continue;
        };

        let old_fm = &old_page.frontmatter.fields;
        let new_fm = &new_page.frontmatter.fields;
        let overlay_fm = &overlay.frontmatter.fields;
        // Keys the upstream changed (added/removed/altered), minus the
        // importer-stamped `layer`.
        let mut changed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (k, v) in new_fm {
            let key = k.as_str().unwrap_or_default().to_owned();
            if key == "layer" {
                continue;
            }
            if old_fm.get(k) != Some(v) {
                changed.insert(key);
            }
        }
        for (k, _) in old_fm {
            let key = k.as_str().unwrap_or_default().to_owned();
            if key != "layer" && new_fm.get(k).is_none() {
                changed.insert(key);
            }
        }
        if old_page.body != new_page.body {
            changed.insert("body".to_owned());
        }
        // Keys the overlay overrides relative to the OLD base.
        for key in changed {
            let overridden = if key == "body" {
                overlay.body != old_page.body
            } else {
                let k = escurel_md::YamlValue::String(key.clone());
                overlay_fm.get(&k).is_some() && overlay_fm.get(&k) != old_fm.get(&k)
            };
            if overridden {
                issues.push(json!({
                    "severity": "error",
                    "code": "rebase_conflict",
                    "location": format!("skill {skill_id} · {key}"),
                    "message": format!(
                        "the tenant overlay overrides `{key}` of skill `{skill_id}` and \
                         `{}` v{} also changes it — review the shadow, then re-run with \
                         acknowledge_conflicts=true",
                        a.manifest.id, a.manifest.version
                    ),
                }));
            }
        }
    }
    // Plan only: everything above (verify, unpack, stamp, collision +
    // conflict scans) ran exactly as a real rebase would; report the
    // would-import / would-remove counts and apply NOTHING. `ok` means
    // "a real run with these SAME arguments would apply" — clean, or
    // conflicted-but-acknowledged; the issues stay listed either way.
    if a.dry_run {
        let old_page_ids = indexer
            .base_page_ids(&a.manifest.id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("rebase_pack orphan scan: {e}")))?;
        let new_ids: std::collections::HashSet<&str> =
            stamped_pages.iter().map(|(id, _)| id.as_str()).collect();
        let would_remove = old_page_ids
            .iter()
            .filter(|id| !new_ids.contains(id.as_str()))
            .count();
        return Ok(json!({
            "ok": issues.is_empty() || a.acknowledge_conflicts,
            "dry_run": true,
            "issues": issues,
            "pack": a.manifest.id,
            "from_version": from_version,
            "to_version": a.manifest.version,
            "would_import": stamped_pages.len(),
            "would_remove": would_remove,
        }));
    }
    if !issues.is_empty() && !a.acknowledge_conflicts {
        return Ok(json!({ "ok": false, "issues": issues }));
    }
    let conflicts_acknowledged = issues.len() as u32;

    // Apply: land the new version, remove orphans, move the pin LAST.
    // Crash-recovery note (agy review): conflicts block BEFORE any write,
    // so human review always happened by this point; a crash inside this
    // block leaves v{N+1} pages with the old pin — recovery is re-running
    // the same rebase (page upsert + orphan removal are idempotent), and
    // the pin never claims a version whose pages didn't fully land.
    let old_page_ids = indexer
        .base_page_ids(&a.manifest.id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("rebase_pack orphan scan: {e}")))?;
    let new_ids: std::collections::HashSet<&str> =
        stamped_pages.iter().map(|(id, _)| id.as_str()).collect();
    let mut pages_imported = 0u32;
    for (page_id, stamped) in &stamped_pages {
        indexer
            .update_page(page_id, stamped)
            .await
            .map_err(|e| JsonRpcError::internal(format!("rebase_pack `{page_id}`: {e}")))?;
        pages_imported += 1;
    }
    let mut pages_removed = 0u32;
    for old_id in &old_page_ids {
        if !new_ids.contains(old_id.as_str()) {
            indexer.remove_page(old_id).await.map_err(|e| {
                JsonRpcError::internal(format!("rebase_pack remove `{old_id}`: {e}"))
            })?;
            pages_removed += 1;
        }
    }
    indexer
        .refresh_fts()
        .await
        .map_err(|e| JsonRpcError::internal(format!("rebase_pack refresh_fts: {e}")))?;
    indexer
        .record_pack_subscription(&escurel_index::pack::PackSubscription {
            pack_id: a.manifest.id.clone(),
            version: a.manifest.version,
            vertical: a.manifest.vertical.clone(),
            publisher: a.manifest.publisher.clone(),
            content_hash: a.manifest.content_hash.clone(),
            signature: a.manifest.signature.clone(),
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("rebase_pack subscription: {e}")))?;

    Ok(json!({
        "ok": true,
        "issues": [],
        "pack": a.manifest.id,
        "from_version": from_version,
        "to_version": a.manifest.version,
        "pages_imported": pages_imported,
        "pages_removed": pages_removed,
        "conflicts_acknowledged": conflicts_acknowledged,
    }))
}

#[derive(Deserialize)]
pub(super) struct UnsubscribePackArgs {
    tenant_id: String,
    pack_id: String,
}

/// Admin: cleanly drop a subscription — every base page the pack landed
/// (so `rebuild` cannot resurrect orphaned base content) AND the pin
/// row. Tenant overlay pages survive untouched; a shadow simply stops
/// shadowing. A later `import_pack` starts from zero.
pub(super) async fn tool_unsubscribe_pack(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: UnsubscribePackArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("unsubscribe_pack: {e}")))?;
    if !crate::pack::is_safe_pack_token(&a.pack_id) {
        return Err(JsonRpcError::internal(
            "pack_id_invalid: not a safe pack id",
        ));
    }
    let indexer = admin_indexer(state)?;
    ensure_tenant_matches(&indexer, &a.tenant_id)?;
    let subs = indexer
        .list_pack_subscriptions()
        .await
        .map_err(|e| JsonRpcError::internal(format!("unsubscribe_pack: {e}")))?;
    if !subs.iter().any(|s| s.pack_id == a.pack_id) {
        return Err(JsonRpcError::internal(format!(
            "pack_not_subscribed: `{}` has no subscription on this node",
            a.pack_id
        )));
    }
    // Admin lifecycle ops on the SAME pack (unsubscribe vs concurrent
    // import/rebase) are not concurrent-safe — same single-operator
    // posture as the import/rebase crash windows: recovery is re-running
    // the idempotent operation (agy review, documented).
    let page_ids = indexer
        .base_page_ids(&a.pack_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("unsubscribe_pack scan: {e}")))?;
    let mut pages_removed = 0u32;
    for page_id in &page_ids {
        indexer
            .remove_page(page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("unsubscribe_pack `{page_id}`: {e}")))?;
        pages_removed += 1;
    }
    indexer
        .refresh_fts()
        .await
        .map_err(|e| JsonRpcError::internal(format!("unsubscribe_pack refresh_fts: {e}")))?;
    // The pin goes LAST — a failed removal leaves the subscription
    // visible so the operator re-runs rather than losing track of a
    // half-removed pack.
    indexer
        .delete_pack_subscription(&a.pack_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("unsubscribe_pack pin: {e}")))?;
    Ok(json!({ "pack": a.pack_id, "pages_removed": pages_removed }))
}

/// Admin: the subscribed packs and their pins (REQ-SUB-01).
pub(super) async fn tool_list_packs(state: &AppState) -> Result<Value, JsonRpcError> {
    let indexer = admin_indexer(state)?;
    let subs = indexer
        .list_pack_subscriptions()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_packs: {e}")))?;
    Ok(json!({
        "packs": subs
            .into_iter()
            .map(|s| json!({
                "pack_id": s.pack_id,
                "version": s.version,
                "vertical": s.vertical,
                "publisher": s.publisher,
                "content_hash": s.content_hash,
            }))
            .collect::<Vec<_>>(),
    }))
}

pub(super) async fn tool_tenant_export(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_export: {e}")))?;
    let store = tenant_store(state)?.clone();
    // Validate before constructing on-disk paths — `tenant_dir` is
    // filesystem-direct and would happily resolve `../other`.
    validate_tenant_id(&a.tenant_id).map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    let tenant_dir = store
        .tenant_dir(&a.tenant_id)
        .ok_or_else(|| JsonRpcError::internal("tenant store has no on-disk path"))?;
    // Spec (storage.md): only canonical markdown is exported.
    let markdown_dir = tenant_dir.join("markdown");
    if !tokio::fs::try_exists(&markdown_dir).await.unwrap_or(false) {
        return Err(JsonRpcError::invalid_params(format!(
            "tenant `{}` not found",
            a.tenant_id
        )));
    }
    // Build the whole tarball in memory on a blocking thread (file
    // I/O + zlib). The MCP transport is one-shot, so we accumulate
    // every chunk rather than streaming.
    const CHUNK: usize = 64 * 1024;
    let bytes = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        tar_gz_into_chunks(&markdown_dir, CHUNK, |chunk| {
            out.extend_from_slice(&chunk);
            Ok(())
        })?;
        Ok(out)
    })
    .await
    .map_err(|e| JsonRpcError::internal(format!("tenant_export join error: {e}")))?
    .map_err(|e| JsonRpcError::internal(format!("tenant_export: {e}")))?;
    let len = bytes.len() as u64;
    // The export-format version + a SHA-256 of the body so a consumer can
    // verify the tarball before treating it as durable (protocol.md §backup).
    let sha256 = sha256_hex(&bytes);
    Ok(json!({
        "format_version": TENANT_EXPORT_FORMAT_VERSION,
        "tarball_b64": B64.encode(&bytes),
        "bytes": len,
        "sha256": sha256,
    }))
}

/// The `tenant_export` tarball format version (bump on any layout change).
pub(super) const TENANT_EXPORT_FORMAT_VERSION: u32 = 1;

/// Lowercase-hex SHA-256 of `bytes`.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

#[derive(Deserialize)]
pub(super) struct TenantImportArgs {
    #[serde(default)]
    tenant_id: String,
    #[serde(default)]
    tarball_b64: String,
}

pub(super) async fn tool_tenant_import(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: TenantImportArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("tenant_import: {e}")))?;
    let store = tenant_store(state)?.clone();
    validate_tenant_id(&a.tenant_id).map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    // The target tenant must exist before import (mirrors gRPC).
    if store
        .get(&a.tenant_id)
        .await
        .map_err(map_admin_err)?
        .is_none()
    {
        return Err(JsonRpcError::invalid_params(format!(
            "tenant `{}` not found",
            a.tenant_id
        )));
    }
    let tenant_dir = store
        .tenant_dir(&a.tenant_id)
        .ok_or_else(|| JsonRpcError::internal("tenant store has no on-disk path"))?;
    let markdown_dir = tenant_dir.join("markdown");
    let buf = B64
        .decode(a.tarball_b64.as_bytes())
        .map_err(|e| JsonRpcError::invalid_params(format!("tarball_b64 is not base64: {e}")))?;
    let bytes_imported = buf.len() as u64;
    tokio::task::spawn_blocking(move || untar_gz_into(&buf, &markdown_dir))
        .await
        .map_err(|e| JsonRpcError::internal(format!("tenant_import join error: {e}")))?
        .map_err(|e| JsonRpcError::internal(format!("tenant_import: {e}")))?;
    to_value(TenantImportResponse { bytes_imported })
}

#[derive(Deserialize)]
pub(super) struct AttachExternalArgs {
    #[serde(default)]
    tenant_id: String,
    #[serde(default)]
    source_url: String,
}

pub(super) async fn tool_attach_external(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AttachExternalArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("attach_external: {e}")))?;
    let indexer = admin_indexer(state)?;
    ensure_tenant_matches(&indexer, &a.tenant_id)?;
    // Reject an unsafe source before it reaches the ATTACH SQL.
    // DuckDB has no parameter binding for ATTACH, so this is the
    // injection boundary (the indexer re-checks defensively).
    if !is_safe_attach_source(&a.source_url) {
        return Err(JsonRpcError::invalid_params(
            "source_url contains an unsafe character (quote, backslash, semicolon, \
             or control char) or is empty"
                .to_owned(),
        ));
    }
    // Derive a safe catalog alias from the source — the caller does
    // not choose it (matches the gRPC contract; the returned
    // `source_id` is this derived alias, not the tenant).
    let alias = derive_attach_alias(&a.source_url).ok_or_else(|| {
        JsonRpcError::invalid_params("could not derive a catalog alias from source_url".to_owned())
    })?;
    indexer
        .attach_external(&alias, &a.source_url)
        .await
        .map_err(|e| JsonRpcError::internal(format!("attach_external: {e}")))?;
    to_value(AttachExternalResponse { source_id: alias })
}

pub(super) async fn tool_embedding_reload(state: &AppState) -> Result<Value, JsonRpcError> {
    // The reloadable seam + the rebuild factory are wired together:
    // without both there is nothing to reload.
    let (reload, factory) = match (&state.embedder_reload, &state.embedder_factory) {
        (Some(r), Some(f)) => (r, f),
        _ => {
            return Err(JsonRpcError::internal("no reloadable embedder configured"));
        }
    };
    let (embedder, model_revision) = factory()
        .await
        .map_err(|e| JsonRpcError::internal(format!("embedding_reload: model load failed: {e}")))?;
    reload.reload(embedder);
    to_value(EmbeddingReloadResponse { model_revision })
}

pub(super) async fn tool_rebuild(state: &AppState, args: Value) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("rebuild: {e}")))?;
    if !a.tenant_id.is_empty() {
        validate_tenant_id(&a.tenant_id)
            .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    }
    let indexer = admin_indexer(state)?;
    // A wrong `tenant_id` must not silently rebuild this gateway's
    // (only) tenant.
    ensure_tenant_matches(&indexer, &a.tenant_id)?;
    // Capture the last (done, total) the progress callback reports.
    // The MCP transport returns the terminal counts rather than a
    // progress stream.
    let last = Arc::new(std::sync::Mutex::new((0u64, 0u64)));
    let sink = Arc::clone(&last);
    indexer
        .rebuild_with_progress(move |p| {
            if let Ok(mut g) = sink.lock() {
                *g = (p.done, p.total);
            }
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("rebuild: {e}")))?;
    let (done, total) = *last.lock().expect("rebuild progress lock");
    to_value(RebuildProgress {
        done,
        total,
        current_page: String::new(),
    })
}

/// Admin: trigger a DuckLake publish + retention GC on demand
/// (DuckLake PR 7). Idempotently re-attaches the lake (safe every call —
/// `ATTACH IF NOT EXISTS`), then [`publish_lake`] using this gateway's
/// shared `last_published_epoch` (so a manual call and the optional
/// periodic [`crate::snapshot_publish::PublishTask`] never duplicate a
/// publish), and — only on an actual (non-skipped) publish — a
/// follow-up [`gc_lake_snapshots`] pass down to `ESCUREL_SNAPSHOT_KEEP`.
/// A GC failure is logged, not surfaced as an error: the publish itself
/// already committed and the response should still report it.
pub(super) async fn tool_publish_snapshot(state: &AppState) -> Result<Value, JsonRpcError> {
    let lake_cfg = state.lake.as_ref().ok_or_else(|| {
        JsonRpcError::publish_unavailable(
            "no DuckLake configured on this gateway (ESCUREL_INDEX_BACKEND != ducklake)",
        )
    })?;
    let indexer = admin_indexer(state)?;
    indexer
        .attach_lake(lake_cfg)
        .await
        .map_err(|e| JsonRpcError::internal(format!("publish_snapshot: attach_lake: {e}")))?;

    let last_epoch = *state
        .last_published_epoch
        .lock()
        .expect("last_published_epoch lock");
    let report = publish_lake(&indexer, lake_cfg, last_epoch)
        .await
        .map_err(|e| JsonRpcError::internal(format!("publish_snapshot: {e}")))?;

    let mut pruned_snapshots = 0u64;
    if !report.skipped {
        *state
            .last_published_epoch
            .lock()
            .expect("last_published_epoch lock") = Some(report.epoch);
        match gc_lake_snapshots(&indexer, lake_cfg, state.snapshot_keep).await {
            Ok(n) => pruned_snapshots = n,
            Err(e) => {
                tracing::warn!(
                    target: "escurel",
                    error = %e,
                    "publish_snapshot: gc_lake_snapshots failed (publish itself committed)"
                );
            }
        }
    }

    to_value(PublishSnapshotResponse {
        snapshot_id: report.snapshot_id,
        epoch: report.epoch,
        pages: report.pages,
        blocks: report.blocks,
        skipped: report.skipped,
        pruned_snapshots,
    })
}

pub(super) async fn tool_compact_lanes(
    state: &AppState,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: TenantIdArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("compact_lanes: {e}")))?;
    validate_tenant_id(&a.tenant_id).map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    let backend = state
        .crdt_backend
        .as_ref()
        .ok_or_else(|| JsonRpcError::internal("server has no crdt_backend wired"))?
        .clone();
    let pages = backend
        .pages_with_snapshots()
        .await
        .map_err(|e| JsonRpcError::internal(format!("compact_lanes: list pages: {e}")))?;
    let mut ops_compacted = 0u64;
    let mut bytes_reclaimed = 0u64;
    for page_id in pages {
        let (ops, bytes) = backend
            .compact_subsumed_ops(&page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("compact_lanes: page `{page_id}`: {e}")))?;
        ops_compacted += ops;
        bytes_reclaimed += bytes;
    }
    to_value(CompactProgress {
        ops_compacted,
        bytes_reclaimed,
    })
}
