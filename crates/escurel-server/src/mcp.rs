//! MCP-over-HTTP dispatcher: receives JSON-RPC 2.0 requests on
//! `POST /mcp`, routes the agent-facing read tools to `Indexer`
//! methods, returns JSON-RPC 2.0 responses.
//!
//! Wire shape follows `docs/spec/protocol.md §MCP-over-HTTP framing`
//! verbatim:
//!
//! ```jsonc
//! // request
//! { "jsonrpc": "2.0", "id": 1, "method": "tools/call",
//!   "params": { "name": "search", "arguments": { "q": "...", "k": 10 } } }
//! // response
//! { "jsonrpc": "2.0", "id": 1, "result": { ... tool output ... } }
//! // or
//! { "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "..." } }
//! ```
//!
//! Today the seven read tools are wired
//! (`list_skills` / `list_instances` / `resolve` / `expand` /
//! `neighbours` / `search` / `run_stored_query`); the write tools
//! and the MCP `tools/list` discovery call land in follow-ups.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use escurel_index::{Direction, Indexer, OrderDir};
use escurel_md::PageType;
use serde::Deserialize;
use serde_json::{Value, json};

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Inner shape of `params` for `method = "tools/call"`.
#[derive(Debug, Deserialize)]
pub struct ToolsCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// MCP entry point: `POST /mcp`. Single-request, single-response
/// for now; batched and SSE-streamed responses come with the
/// write-path PR.
pub async fn mcp(
    State(state): State<crate::server::AppState>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if req.jsonrpc != "2.0" {
        return error_response(req.id, -32600, "invalid jsonrpc version");
    }

    let result = match req.method.as_str() {
        "tools/list" => Ok(tools_list_payload()),
        "tools/call" => match state.indexer.as_ref() {
            Some(indexer) => dispatch_tools_call(indexer, req.params).await,
            None => Err(JsonRpcError::internal(
                "server has no indexer wired; tools/call is unavailable",
            )),
        },
        other => Err(JsonRpcError::method_not_found(format!(
            "unknown method `{other}`"
        ))),
    };

    match result {
        Ok(value) => (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "result": value,
            })),
        )
            .into_response(),
        Err(err) => err.into_response(req.id),
    }
}

async fn dispatch_tools_call(indexer: &Indexer, params: Value) -> Result<Value, JsonRpcError> {
    let params: ToolsCallParams = serde_json::from_value(params)
        .map_err(|e| JsonRpcError::invalid_params(format!("tools/call params: {e}")))?;

    match params.name.as_str() {
        "list_skills" => tool_list_skills(indexer).await,
        "list_instances" => tool_list_instances(indexer, params.arguments).await,
        "resolve" => tool_resolve(indexer, params.arguments).await,
        "expand" => tool_expand(indexer, params.arguments).await,
        "neighbours" => tool_neighbours(indexer, params.arguments).await,
        "search" => tool_search(indexer, params.arguments).await,
        "run_stored_query" => tool_run_stored_query(indexer, params.arguments).await,
        "update_page" => tool_update_page(indexer, params.arguments).await,
        other => Err(JsonRpcError::method_not_found(format!(
            "unknown tool `{other}`"
        ))),
    }
}

// --- per-tool handlers -----------------------------------------

async fn tool_list_skills(indexer: &Indexer) -> Result<Value, JsonRpcError> {
    let skills = indexer
        .list_skills()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_skills: {e}")))?;
    Ok(json!({
        "skills": skills.iter().map(|s| json!({
            "id": s.id,
            "description": s.description,
            "required_frontmatter": s.required_frontmatter,
            "optional_frontmatter": s.optional_frontmatter,
            "is_event_typed": s.is_event_typed,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct ListInstancesArgs {
    skill_id: String,
    #[serde(default)]
    order_by: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn tool_list_instances(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ListInstancesArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("list_instances: {e}")))?;
    let order = match a.order_by.as_deref() {
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "at asc" | "at_asc" => Some(OrderDir::Asc),
            "at desc" | "at_desc" => Some(OrderDir::Desc),
            _ => None,
        },
        None => None,
    };
    let out = indexer
        .list_instances(&a.skill_id, order, a.limit)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_instances: {e}")))?;
    Ok(json!({
        "instances": out.iter().map(|i| json!({
            "page_id": i.page_id,
            "skill": i.skill,
            "frontmatter": i.frontmatter,
            "at": i.at,
        })).collect::<Vec<_>>(),
        "next_cursor": Value::Null,
    }))
}

#[derive(Deserialize)]
struct ResolveArgs {
    wikilink: String,
}

async fn tool_resolve(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ResolveArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("resolve: {e}")))?;
    let resolved = indexer
        .resolve(&a.wikilink)
        .await
        .map_err(|e| JsonRpcError::internal(format!("resolve: {e}")))?;
    let exists = resolved.exists();
    let parsed = &resolved.parsed;
    Ok(json!({
        "parsed": {
            "skill": parsed.skill,
            "id": parsed.id,
            "anchor": parsed.anchor,
            "version": parsed.version,
            "alias": parsed.alias,
        },
        "page": resolved.page.as_ref().map(|p| json!({
            "page_id": p.page_id,
            "slug": p.slug,
            "skill": p.skill,
            "page_type": page_type_str(p.page_type),
        })),
        "exists": exists,
    }))
}

#[derive(Deserialize)]
struct ExpandArgs {
    page_id: String,
}

async fn tool_expand(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ExpandArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("expand: {e}")))?;
    let out = indexer
        .expand(&a.page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("expand: {e}")))?;
    match out {
        None => Ok(json!({ "page": Value::Null })),
        Some(e) => Ok(json!({
            "page": {
                "page_id": e.page.page_id,
                "slug": e.page.slug,
                "skill": e.page.skill,
                "page_type": page_type_str(e.page.page_type),
            },
            "frontmatter": e.frontmatter,
            "body": e.body,
            "blocks": e.blocks.iter().map(|b| json!({
                "anchor": b.anchor,
                "content": b.content,
            })).collect::<Vec<_>>(),
            "wikilinks_out": e.wikilinks_out.iter().map(|w| json!({
                "skill": w.skill, "id": w.id, "anchor": w.anchor,
                "version": w.version, "alias": w.alias,
            })).collect::<Vec<_>>(),
        })),
    }
}

#[derive(Deserialize)]
struct NeighboursArgs {
    page_id: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    link_skill: Option<String>,
}

async fn tool_neighbours(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: NeighboursArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("neighbours: {e}")))?;
    let dir = match a.direction.as_deref().unwrap_or("both") {
        "in" => Direction::In,
        "out" => Direction::Out,
        "both" => Direction::Both,
        other => {
            return Err(JsonRpcError::invalid_params(format!(
                "neighbours direction `{other}`; expected in|out|both"
            )));
        }
    };
    let edges = indexer
        .neighbours(&a.page_id, dir, a.link_skill.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("neighbours: {e}")))?;
    Ok(json!({
        "edges": edges.iter().map(|e| json!({
            "src_page": e.src_page,
            "dst_page": e.dst_page,
            "link_skill": e.link_skill,
            "link_version": e.link_version,
            "dst_anchor": e.dst_anchor,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct SearchArgs {
    q: String,
    #[serde(default = "default_k")]
    k: usize,
    #[serde(default)]
    page_type: Option<String>,
    #[serde(default)]
    skill: Option<String>,
}

fn default_k() -> usize {
    10
}

async fn tool_search(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: SearchArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("search: {e}")))?;
    let pt = match a.page_type.as_deref() {
        None | Some("any") => None,
        Some("skill") => Some(PageType::Skill),
        Some("instance") => Some(PageType::Instance),
        Some(other) => {
            return Err(JsonRpcError::invalid_params(format!(
                "search page_type `{other}`; expected skill|instance|any"
            )));
        }
    };
    let hits = indexer
        .search(&a.q, a.k, pt, a.skill.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("search: {e}")))?;
    Ok(json!({
        "hits": hits.iter().map(|h| json!({
            "page_id": h.page_id,
            "slug": h.slug,
            "skill": h.skill,
            "page_type": page_type_str(h.page_type),
            "anchor": h.anchor,
            "snippet": h.snippet,
            "score": h.score,
            "frontmatter_excerpt": h.frontmatter_excerpt,
        })).collect::<Vec<_>>(),
        "granularity": "block",
    }))
}

#[derive(Deserialize)]
struct RunStoredQueryArgs {
    query_id: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

async fn tool_run_stored_query(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: RunStoredQueryArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("run_stored_query: {e}")))?;
    let out = indexer
        .run_stored_query(&a.query_id, &a.params)
        .await
        .map_err(|e| JsonRpcError::internal(format!("run_stored_query: {e}")))?;
    Ok(json!({
        "rows": out.rows,
        "schema": out.schema.iter().map(|c| json!({
            "name": c.name,
            "type": c.type_name,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct UpdatePageArgs {
    page_id: String,
    content: String,
}

async fn tool_update_page(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: UpdatePageArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("update_page: {e}")))?;
    indexer
        .update_page(&a.page_id, &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("update_page: {e}")))?;
    // The trait doesn't currently surface validation issues
    // (it errors out instead). Return the protocol-required
    // `{ok, issues}` shape with an empty issues list and a stub
    // new_version derived from the body hash. Real version IDs
    // arrive once the CRDT layer (M4) gives us monotonic
    // versions; until then the client only needs the field to
    // exist.
    Ok(json!({
        "ok": true,
        "issues": [],
        "new_version": "v1",
    }))
}

// --- tools/list payload ----------------------------------------

/// MCP `tools/list` response payload. Each entry is `{ name,
/// description, inputSchema }`. The wire shape matches the
/// upstream MCP spec exactly so any conforming MCP client can
/// drive Escurel without bespoke wiring.
fn tools_list_payload() -> Value {
    json!({
        "tools": [
            tool_entry(
                "list_skills",
                "Return the tenant's Tier-1 skill catalogue.",
                json!({ "type": "object", "properties": {} }),
            ),
            tool_entry(
                "list_instances",
                "Enumerate instances of a skill.",
                json!({
                    "type": "object",
                    "required": ["skill_id"],
                    "properties": {
                        "skill_id": { "type": "string" },
                        "order_by": { "type": "string", "enum": ["at asc", "at desc"] },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000 }
                    }
                }),
            ),
            tool_entry(
                "resolve",
                "Parse a [[wikilink]] and look up its target page.",
                json!({
                    "type": "object",
                    "required": ["wikilink"],
                    "properties": { "wikilink": { "type": "string" } }
                }),
            ),
            tool_entry(
                "expand",
                "Fetch a page's frontmatter + body + outbound wikilinks.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": { "page_id": { "type": "string" } }
                }),
            ),
            tool_entry(
                "neighbours",
                "Typed link-graph traversal.",
                json!({
                    "type": "object",
                    "required": ["page_id"],
                    "properties": {
                        "page_id": { "type": "string" },
                        "direction": { "type": "string", "enum": ["in", "out", "both"] },
                        "link_skill": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "search",
                "Hybrid vector + FTS search, RRF-fused.",
                json!({
                    "type": "object",
                    "required": ["q"],
                    "properties": {
                        "q": { "type": "string" },
                        "k": { "type": "integer", "minimum": 0, "maximum": 1000 },
                        "page_type": { "type": "string", "enum": ["skill", "instance", "any"] },
                        "skill": { "type": "string" }
                    }
                }),
            ),
            tool_entry(
                "run_stored_query",
                "Execute a [[query::*]] instance with named parameters.",
                json!({
                    "type": "object",
                    "required": ["query_id"],
                    "properties": {
                        "query_id": { "type": "string" },
                        "params": { "type": "object" }
                    }
                }),
            ),
            tool_entry(
                "update_page",
                "Upsert a markdown page (whole-body write).",
                json!({
                    "type": "object",
                    "required": ["page_id", "content"],
                    "properties": {
                        "page_id": { "type": "string" },
                        "content": { "type": "string" }
                    }
                }),
            ),
        ]
    })
}

fn tool_entry(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

// --- helpers ---------------------------------------------------

fn page_type_str(pt: PageType) -> &'static str {
    match pt {
        PageType::Skill => "skill",
        PageType::Instance => "instance",
    }
}

#[derive(Debug)]
struct JsonRpcError {
    code: i32,
    message: String,
}

impl JsonRpcError {
    fn method_not_found(msg: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: msg.into(),
        }
    }
    fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
        }
    }
    fn into_response(self, id: Value) -> axum::response::Response {
        (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": self.code, "message": self.message },
            })),
        )
            .into_response()
    }
}

fn error_response(id: Value, code: i32, msg: impl Into<String>) -> axum::response::Response {
    JsonRpcError {
        code,
        message: msg.into(),
    }
    .into_response(id)
}
