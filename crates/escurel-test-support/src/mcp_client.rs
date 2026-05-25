//! Typed JSON-RPC client for the MCP-over-HTTP transport.
//!
//! This is the test-side mirror of `escurel-client::Client`: same
//! method set, same request/response types, but talks to `POST
//! /mcp` carrying JSON-RPC 2.0 envelopes instead of the native
//! gRPC wire. Constructed by [`crate::EscurelProcess::mcp_client`];
//! pre-loaded with a bearer token minted via
//! [`crate::EscurelProcess::mint_token`] so the test doesn't manage
//! tokens directly.
//!
//! The typed methods mirror `Client`'s set 1:1 — `list_skills`,
//! `list_instances`, `resolve`, `expand`, `neighbours`, `search`,
//! `run_stored_query`, `update_page`. The wire shape sits on
//! `docs/spec/protocol.md`'s MCP framing (verbatim JSON tool
//! result), but the typed surface returns the same proto-generated
//! response types as `Client` so a test can swap transports
//! without changing call sites.

use std::sync::atomic::{AtomicI64, Ordering};

use escurel_client::{
    Edge, ExpandBlock, ExpandRequest, ExpandResponse, InstanceInfo, ListInstancesRequest,
    ListInstancesResponse, ListSkillsRequest, ListSkillsResponse, NeighboursRequest,
    NeighboursResponse, PageRef, ResolveRequest, ResolveResponse, RunStoredQueryRequest,
    RunStoredQueryResponse, SearchHit, SearchRequest, SearchResponse, Skill, StoredQueryColumn,
    UpdatePageRequest, UpdatePageResponse, ValidationIssue, WikilinkParsed,
};
use serde_json::{Value, json};

/// Error variants returned by [`McpTestClient`]. Mirrors the wire-
/// failure modes a downstream test cares about: transport, HTTP
/// status, JSON-RPC error envelope, JSON decode.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("jsonrpc error: code={code} message={message}")]
    JsonRpc { code: i64, message: String },
    #[error("response missing `result` field: {body}")]
    MissingResult { body: String },
    #[error("response decode failed: {source}")]
    Decode {
        #[source]
        source: serde_json::Error,
    },
}

/// JSON-RPC client over `POST /mcp`. Cheap to clone — wraps a
/// `reqwest::Client` (already arc-internal) and a string URL +
/// bearer.
#[derive(Clone)]
pub struct McpTestClient {
    http: reqwest::Client,
    mcp_url: String,
    bearer: Option<String>,
    next_id: std::sync::Arc<AtomicI64>,
}

impl std::fmt::Debug for McpTestClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately do not print `bearer` — it carries a JWT.
        f.debug_struct("McpTestClient")
            .field("mcp_url", &self.mcp_url)
            .finish_non_exhaustive()
    }
}

impl McpTestClient {
    pub(crate) fn new(mcp_url: String, bearer: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            mcp_url,
            bearer,
            next_id: std::sync::Arc::new(AtomicI64::new(1)),
        }
    }

    /// Hybrid vector + FTS search over `POST /mcp`.
    pub async fn search(&self, req: SearchRequest) -> Result<SearchResponse, McpError> {
        let raw = self.call("search", search_args(&req)).await?;
        Ok(decode_search(raw))
    }

    /// Resolve a `[[wikilink]]` to a target page.
    pub async fn resolve(&self, req: ResolveRequest) -> Result<ResolveResponse, McpError> {
        let raw = self
            .call("resolve", json!({ "wikilink": req.wikilink }))
            .await?;
        Ok(decode_resolve(raw))
    }

    /// Fetch a page's frontmatter, body, and outbound wikilinks.
    pub async fn expand(&self, req: ExpandRequest) -> Result<ExpandResponse, McpError> {
        let mut args = json!({ "page_id": req.page_id });
        if !req.anchor.is_empty() {
            args["anchor"] = json!(req.anchor);
        }
        if !req.version.is_empty() {
            args["version"] = json!(req.version);
        }
        let raw = self.call("expand", args).await?;
        Ok(decode_expand(raw))
    }

    /// Typed link-graph traversal.
    pub async fn neighbours(&self, req: NeighboursRequest) -> Result<NeighboursResponse, McpError> {
        let mut args = json!({ "page_id": req.page_id });
        if !req.direction.is_empty() {
            args["direction"] = json!(req.direction);
        }
        if !req.link_skill.is_empty() {
            args["link_skill"] = json!(req.link_skill);
        }
        let raw = self.call("neighbours", args).await?;
        Ok(decode_neighbours(raw))
    }

    /// Return the tenant's Tier-1 skill catalogue.
    pub async fn list_skills(
        &self,
        _req: ListSkillsRequest,
    ) -> Result<ListSkillsResponse, McpError> {
        let raw = self.call("list_skills", json!({})).await?;
        Ok(decode_list_skills(raw))
    }

    /// Enumerate instances of a skill.
    pub async fn list_instances(
        &self,
        req: ListInstancesRequest,
    ) -> Result<ListInstancesResponse, McpError> {
        let mut args = json!({ "skill_id": req.skill });
        if !req.order_by_at.is_empty() {
            args["order_by"] = json!(format!("at {}", req.order_by_at));
        }
        if req.limit > 0 {
            args["limit"] = json!(req.limit);
        }
        let raw = self.call("list_instances", args).await?;
        Ok(decode_list_instances(raw))
    }

    /// Execute a `[[query::<id>]]` instance with named parameters.
    pub async fn run_stored_query(
        &self,
        req: RunStoredQueryRequest,
    ) -> Result<RunStoredQueryResponse, McpError> {
        // `RunStoredQueryRequest::params_json` is a stringified JSON
        // object on the gRPC wire; the MCP tool's `arguments` takes
        // a real JSON object under `params`. Parse the string and
        // re-emit so the two transports take the same input shape.
        let params: Value = if req.params_json.is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&req.params_json).map_err(|source| McpError::Decode { source })?
        };
        let raw = self
            .call(
                "run_stored_query",
                json!({ "query_id": req.query_id, "params": params }),
            )
            .await?;
        decode_run_stored_query(raw)
    }

    /// Upsert a markdown page (the public write path).
    pub async fn update_page(
        &self,
        req: UpdatePageRequest,
    ) -> Result<UpdatePageResponse, McpError> {
        let raw = self
            .call(
                "update_page",
                json!({ "page_id": req.page_id, "content": req.content }),
            )
            .await?;
        Ok(decode_update_page(raw))
    }

    /// Low-level JSON-RPC `tools/call` driver. Returns the inner
    /// `result` JSON value, or maps the JSON-RPC error envelope to
    /// [`McpError::JsonRpc`]. Public so a downstream test can
    /// exercise a method this façade doesn't yet wrap (or pin a
    /// raw assertion against the wire bytes).
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        });
        let mut req = self.http.post(&self.mcp_url).json(&envelope);
        if let Some(b) = &self.bearer {
            req = req.header("authorization", format!("Bearer {b}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body_text = resp.text().await?;
        if !status.is_success() {
            return Err(McpError::Http {
                status: status.as_u16(),
                body: body_text,
            });
        }
        let body: Value =
            serde_json::from_str(&body_text).map_err(|source| McpError::Decode { source })?;
        if let Some(err) = body.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            return Err(McpError::JsonRpc { code, message });
        }
        body.get("result")
            .cloned()
            .ok_or(McpError::MissingResult { body: body_text })
    }
}

// --- per-tool argument shaping --------------------------------

fn search_args(req: &SearchRequest) -> Value {
    let mut args = json!({ "q": req.q });
    if req.k > 0 {
        args["k"] = json!(req.k);
    }
    if !req.skill.is_empty() {
        args["skill"] = json!(req.skill);
    }
    if !req.page_type.is_empty() {
        args["page_type"] = json!(req.page_type);
    }
    args
}

// --- per-tool result decoding ---------------------------------
//
// The MCP wire shape is JSON-keyed; the proto-generated response
// types are flat structs. Decode field-by-field so a backend
// payload never silently drops on the floor.

fn decode_list_skills(v: Value) -> ListSkillsResponse {
    let skills = v
        .get("skills")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|s| Skill {
            id: opt_str(&s, "id"),
            description: opt_str(&s, "description"),
            required_frontmatter: str_array(&s, "required_frontmatter"),
            optional_frontmatter: str_array(&s, "optional_frontmatter"),
            is_event_typed: s
                .get("is_event_typed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect();
    ListSkillsResponse { skills }
}

fn decode_list_instances(v: Value) -> ListInstancesResponse {
    let instances = v
        .get("instances")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|i| InstanceInfo {
            page_id: opt_str(&i, "page_id"),
            skill: opt_str(&i, "skill"),
            frontmatter_json: i
                .get("frontmatter")
                .map(|fm| fm.to_string())
                .unwrap_or_default(),
            at: opt_str(&i, "at"),
        })
        .collect();
    ListInstancesResponse { instances }
}

fn decode_resolve(v: Value) -> ResolveResponse {
    let exists = v.get("exists").and_then(Value::as_bool).unwrap_or(false);
    let page = v.get("page").and_then(page_ref_from_value);
    let parsed = v.get("parsed").map(|p| WikilinkParsed {
        skill: opt_str(p, "skill"),
        id: opt_str(p, "id"),
        anchor: opt_str(p, "anchor"),
        version: opt_str(p, "version"),
        alias: opt_str(p, "alias"),
    });
    ResolveResponse {
        parsed,
        page,
        exists,
    }
}

fn page_ref_from_value(v: &Value) -> Option<PageRef> {
    if !v.is_object() {
        return None;
    }
    Some(PageRef {
        page_id: opt_str(v, "page_id"),
        slug: opt_str(v, "slug"),
        skill: opt_str(v, "skill"),
        page_type: opt_str(v, "page_type"),
    })
}

fn decode_expand(v: Value) -> ExpandResponse {
    let page = v.get("page").and_then(page_ref_from_value);
    let frontmatter_json = v
        .get("frontmatter")
        .map(|f| f.to_string())
        .unwrap_or_default();
    let body = opt_str(&v, "body");
    let blocks = v
        .get("blocks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|b| ExpandBlock {
            anchor: opt_str(&b, "anchor"),
            content: opt_str(&b, "content"),
        })
        .collect();
    let wikilinks_out = v
        .get("wikilinks_out")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|w| WikilinkParsed {
            skill: opt_str(&w, "skill"),
            id: opt_str(&w, "id"),
            anchor: opt_str(&w, "anchor"),
            version: opt_str(&w, "version"),
            alias: opt_str(&w, "alias"),
        })
        .collect();
    ExpandResponse {
        page,
        frontmatter_json,
        body,
        blocks,
        wikilinks_out,
        snapshot_version: String::new(),
    }
}

fn decode_neighbours(v: Value) -> NeighboursResponse {
    let edges = v
        .get("edges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|e| Edge {
            src_page: opt_str(&e, "src_page"),
            dst_page: opt_str(&e, "dst_page"),
            link_skill: opt_str(&e, "link_skill"),
            link_version: opt_str(&e, "link_version"),
            dst_anchor: opt_str(&e, "dst_anchor"),
        })
        .collect();
    NeighboursResponse { edges }
}

fn decode_search(v: Value) -> SearchResponse {
    let granularity = opt_str(&v, "granularity");
    let hits = v
        .get("hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|h| SearchHit {
            page_id: opt_str(&h, "page_id"),
            slug: opt_str(&h, "slug"),
            skill: opt_str(&h, "skill"),
            page_type: opt_str(&h, "page_type"),
            anchor: opt_str(&h, "anchor"),
            snippet: opt_str(&h, "snippet"),
            score: h.get("score").and_then(Value::as_f64).unwrap_or(0.0),
            frontmatter_excerpt_json: h
                .get("frontmatter_excerpt")
                .map(|fx| fx.to_string())
                .unwrap_or_default(),
        })
        .collect();
    // The MCP wire only carries `hits` + `granularity`. Inject the
    // proto-shape response with these two fields filled.
    let _ = granularity.clone(); // ensure the var is used
    SearchResponse {
        hits,
        // The proto has no `granularity` field — the MCP one is a
        // bit of soft metadata the gateway emits. Keep it out of
        // the typed response and rely on the proto's `hits` for
        // assertions, plus the bare `granularity` field below for
        // back-compat checks via the typed surface.
        granularity,
    }
}

fn decode_run_stored_query(v: Value) -> Result<RunStoredQueryResponse, McpError> {
    let schema = v
        .get("schema")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|c| StoredQueryColumn {
            name: opt_str(&c, "name"),
            type_name: opt_str(&c, "type"),
        })
        .collect();
    let rows_json = v
        .get("rows")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
        .to_string();
    Ok(RunStoredQueryResponse { rows_json, schema })
}

fn decode_update_page(v: Value) -> UpdatePageResponse {
    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let issues = v
        .get("issues")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|i| ValidationIssue {
            code: opt_str(&i, "code"),
            message: opt_str(&i, "message"),
            anchor: opt_str(&i, "anchor"),
        })
        .collect();
    let new_version = opt_str(&v, "new_version");
    UpdatePageResponse {
        ok,
        issues,
        new_version,
    }
}

// --- small helpers --------------------------------------------

fn opt_str(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

fn str_array(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
