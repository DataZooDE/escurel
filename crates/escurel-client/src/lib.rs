//! Typed MCP-over-HTTP client for the Escurel v1 surface.
//!
//! This crate is the *typed wrapper* downstream applications import
//! into their backend. It is a leaf crate: it depends on
//! [`escurel-types`] (the serde wire-contract structs), `reqwest` for
//! the JSON-RPC-over-HTTP transport, and `tokio-tungstenite` for the
//! live-session WebSocket — and nothing else from the escurel
//! workspace. In particular it does **not** pull in `escurel-server`
//! — that crate's dependency tree includes DuckDB, candle, and the
//! indexer; none of that has any business in an application's binary.
//!
//! See [`docs/spec/dx.md`](../../docs/spec/dx.md) §"Client crate for
//! the app's backend" for the wire contract.
//!
//! # Transport
//!
//! The client speaks **MCP-over-HTTP**: each method POSTs a JSON-RPC
//! 2.0 `tools/call` envelope to `<base>/mcp`, carrying the bearer
//! token as `Authorization: Bearer <jwt>`. The live co-editing channel
//! (`live_session`) speaks the WebSocket framing on `<base>/ws`.
//!
//! # Example
//!
//! ```no_run
//! use escurel_client::{Client, ListSkillsRequest};
//! use secrecy::SecretString;
//!
//! # async fn run() -> Result<(), escurel_client::Error> {
//! let token = SecretString::from("eyJ…");
//! let client = Client::connect("http://127.0.0.1:8080", token).await?;
//! let skills = client.list_skills(ListSkillsRequest::default()).await?;
//! for s in skills.skills {
//!     println!("{}: {}", s.id, s.description);
//! }
//! # Ok(())
//! # }
//! ```

mod admin;
mod error;
mod transport;

pub use admin::AdminClient;
// Admin-surface request/response types, re-exported so operators never
// pin `escurel-types` directly (parallels the agent re-exports below).
pub use admin::{
    AttachExternalRequest, AttachExternalResponse, AuditRequest, AuditResponse,
    CompactLanesRequest, CompactProgress, DeleteChatHistoryRequest, DeleteChatHistoryResponse,
    EmbeddingReloadRequest, EmbeddingReloadResponse, HealthRequest, HealthResponse,
    QuotaGetRequest, QuotaGetResponse, RebuildProgress, RebuildRequest, TenantCreateRequest,
    TenantCreateResponse, TenantDeleteRequest, TenantDeleteResponse, TenantExportRequest,
    TenantGetRequest, TenantGetResponse, TenantImportResponse, TenantListRequest,
    TenantListResponse, TenantUpdateRequest, TenantUpdateResponse,
};
pub use error::{Error, JSONRPC_ADMIN_REQUIRED};

// Re-export the request/response types the downstream caller needs so
// they never pin `escurel-types` directly. Covers the agent surface
// from `docs/spec/dx.md` §"Client crate for the app's backend", the
// M7 event-sourcing types (capture / inbox / events / assign), the
// per-chat-group history types, `validate`, and the live-session
// frames. These are the same names the old gRPC client re-exported
// from `escurel_proto::v1`, now sourced from `escurel-types`.
pub use escurel_types::{
    AppendMessageRequest, AppendMessageResponse, AssignEventRequest, AssignEventResponse,
    CaptureEventRequest, ChatMessage, Edge, Event, ExpandBlock, ExpandRequest, ExpandResponse,
    InstanceInfo, ListEventsRequest, ListEventsResponse, ListInboxRequest, ListInboxResponse,
    ListInstancesRequest, ListInstancesResponse, ListMessagesRequest, ListMessagesResponse,
    ListSkillsRequest, ListSkillsResponse, LiveAck, LiveOp, NeighboursRequest, NeighboursResponse,
    PageRef, QueryInstanceRequest, QueryInstanceResponse, ResolveRequest, ResolveResponse,
    RunStoredQueryRequest, RunStoredQueryResponse, SearchHit, SearchRequest, SearchResponse, Skill,
    StoredQueryColumn, TenantSpec, UpdatePageRequest, UpdatePageResponse, ValidateRequest,
    ValidateResponse, ValidationIssue, WikilinkParsed,
};
// #247 tenant lifecycle/quota/embedding sub-types.
pub use escurel_types::{EmbeddingSpec, QuotaOverride, TenantStatus};
// Re-exported so callers don't need to depend on `secrecy` directly
// just to spell out a token. Keeping the version in sync with this
// crate's `Cargo.toml` is part of the semver contract.
pub use secrecy::SecretString;

use serde_json::{Value, json};

use crate::transport::McpTransport;

/// Typed MCP-over-HTTP client for the Escurel v1 agent surface.
///
/// `Client` is opaque on purpose: the underlying HTTP transport and
/// the bearer token are private; the only public knobs are the
/// constructor and the per-tool methods. This keeps the semver surface
/// small.
///
/// The bearer token lives inside a [`secrecy::SecretString`] and is
/// never returned by any accessor, nor included in the type's `Debug`
/// output.
#[derive(Clone)]
pub struct Client {
    transport: McpTransport,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately do not print the transport's bearer — it carries
        // the JWT. The crate's invariant is that no tooling that calls
        // `format!("{client:?}")` (logs, panic-traces, `dbg!`) ever
        // leaks the token.
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// Dial the gateway at `endpoint` (the HTTP base, e.g.
    /// `http://127.0.0.1:8080`) and authenticate subsequent tool calls
    /// with `token` as the bearer.
    ///
    /// No network round-trip happens here — the first request is what
    /// surfaces a connection failure as [`Error::Transport`].
    ///
    /// Errors:
    /// - [`Error::InvalidEndpoint`] if `endpoint` is not a valid base URL.
    /// - [`Error::InvalidToken`] if `token` contains bytes that are not
    ///   legal in an HTTP header value.
    pub async fn connect(endpoint: &str, token: SecretString) -> Result<Self, Error> {
        Ok(Self {
            transport: McpTransport::new(endpoint, token)?,
        })
    }

    /// Hybrid vector + FTS search. See `protocol.md` §search.
    pub async fn search(&self, req: SearchRequest) -> Result<SearchResponse, Error> {
        let mut args = json!({ "q": req.q });
        if req.k > 0 {
            args["k"] = json!(req.k);
        }
        if !req.granularity.is_empty() {
            args["granularity"] = json!(req.granularity);
        }
        if !req.page_type.is_empty() {
            args["page_type"] = json!(req.page_type);
        }
        if !req.skill.is_empty() {
            args["skill"] = json!(req.skill);
        }
        // Forward the optional refinement fields the server's `search`
        // tool honours: frontmatter post-filter, time-travel cut, and
        // scenario overlay. Omitting these (the prior behaviour)
        // silently returned unfiltered/base results.
        if !req.filter.is_null() {
            args["filter"] = req.filter.clone();
        }
        if !req.as_of.is_empty() {
            args["as_of"] = json!(req.as_of);
        }
        if !req.scenario.is_empty() {
            args["scenario"] = json!(req.scenario);
        }
        if !req.page_id.is_empty() {
            args["page_id"] = json!(req.page_id);
        }
        self.transport.call_typed("search", args).await
    }

    /// Parse a `[[wikilink]]` and look up its target page.
    pub async fn resolve(&self, req: ResolveRequest) -> Result<ResolveResponse, Error> {
        self.transport
            .call_typed("resolve", json!({ "wikilink": req.wikilink }))
            .await
    }

    /// Fetch a page's frontmatter, body, and outbound wikilinks.
    pub async fn expand(&self, req: ExpandRequest) -> Result<ExpandResponse, Error> {
        let mut args = json!({ "page_id": req.page_id });
        if !req.anchor.is_empty() {
            args["anchor"] = json!(req.anchor);
        }
        if !req.version.is_empty() {
            args["version"] = json!(req.version);
        }
        if req.full {
            args["full"] = json!(true);
        }
        self.transport.call_typed("expand", args).await
    }

    /// Typed link-graph traversal.
    pub async fn neighbours(&self, req: NeighboursRequest) -> Result<NeighboursResponse, Error> {
        let mut args = json!({ "page_id": req.page_id });
        if !req.direction.is_empty() {
            args["direction"] = json!(req.direction);
        }
        if !req.link_skill.is_empty() {
            args["link_skill"] = json!(req.link_skill);
        }
        self.transport.call_typed("neighbours", args).await
    }

    /// Return the tenant's Tier-1 skill catalogue.
    pub async fn list_skills(&self, _req: ListSkillsRequest) -> Result<ListSkillsResponse, Error> {
        self.transport.call_typed("list_skills", json!({})).await
    }

    /// Enumerate instances of a skill. The server's `arguments` key for
    /// the skill is `skill_id`; the typed request field is `skill`.
    pub async fn list_instances(
        &self,
        req: ListInstancesRequest,
    ) -> Result<ListInstancesResponse, Error> {
        let mut args = json!({ "skill_id": req.skill });
        if !req.order_by_at.is_empty() {
            args["order_by"] = json!(format!("at {}", req.order_by_at));
        }
        if req.limit > 0 {
            args["limit"] = json!(req.limit);
        }
        if !req.frontmatter_key.is_empty() {
            args["frontmatter_key"] = json!(req.frontmatter_key);
            args["frontmatter_value"] = json!(req.frontmatter_value);
        }
        self.transport.call_typed("list_instances", args).await
    }

    /// Execute a `[[query::<id>]]` instance with named parameters.
    pub async fn run_stored_query(
        &self,
        req: RunStoredQueryRequest,
    ) -> Result<RunStoredQueryResponse, Error> {
        let params = if req.params.is_null() {
            json!({})
        } else {
            req.params
        };
        self.transport
            .call_typed(
                "run_stored_query",
                json!({ "query_id": req.query_id, "params": params }),
            )
            .await
    }

    /// Run a `[[query::<id>]]` report against its `target` sql_view
    /// instance's view, binding `params` as prepared-statement values
    /// (issue #205). The per-instance ACL gates the target, fail-closed.
    pub async fn query_instance(
        &self,
        req: QueryInstanceRequest,
    ) -> Result<QueryInstanceResponse, Error> {
        let params = if req.params.is_null() {
            json!({})
        } else {
            req.params
        };
        self.transport
            .call_typed(
                "query_instance",
                json!({ "ref": req.query_ref, "params": params }),
            )
            .await
    }

    /// Dry-run the indexer's validation pipeline over draft `content`
    /// without committing. See `protocol.md` §validate.
    pub async fn validate(&self, req: ValidateRequest) -> Result<ValidateResponse, Error> {
        let mut args = json!({ "content": req.content });
        if !req.as_page_id.is_empty() {
            args["as_page_id"] = json!(req.as_page_id);
        }
        self.transport.call_typed("validate", args).await
    }

    /// Upsert a markdown page (the public write path).
    pub async fn update_page(&self, req: UpdatePageRequest) -> Result<UpdatePageResponse, Error> {
        self.transport
            .call_typed(
                "update_page",
                json!({ "page_id": req.page_id, "content": req.content }),
            )
            .await
    }

    /// Append a message to a chat-group's conversation history
    /// (M-Chat, issue #63). `chat_group_id` is opaque to escurel — the
    /// consumer owns the identifier scheme.
    pub async fn append_message(
        &self,
        req: AppendMessageRequest,
    ) -> Result<AppendMessageResponse, Error> {
        let mut args = json!({
            "chat_group_id": req.chat_group_id,
            "role": req.role,
            "content": req.content,
            "embed": req.embed,
        });
        if !req.author.is_empty() {
            args["author"] = json!(req.author);
        }
        if !req.ts.is_empty() {
            args["ts"] = json!(req.ts);
        }
        if !req.msg_id.is_empty() {
            args["msg_id"] = json!(req.msg_id);
        }
        if !req.metadata.is_null() {
            args["metadata"] = req.metadata;
        }
        self.transport.call_typed("append_message", args).await
    }

    /// Read back a chat-group's history time-ordered. `since` is
    /// inclusive, `until` is exclusive; `direction` defaults to `desc`
    /// (newest first) when omitted. Pass `cursor` from the previous
    /// response's `next_cursor` to continue paging.
    pub async fn list_messages(
        &self,
        req: ListMessagesRequest,
    ) -> Result<ListMessagesResponse, Error> {
        let mut args = json!({ "chat_group_id": req.chat_group_id });
        if !req.since.is_empty() {
            args["since"] = json!(req.since);
        }
        if !req.until.is_empty() {
            args["until"] = json!(req.until);
        }
        if req.limit > 0 {
            args["limit"] = json!(req.limit);
        }
        if !req.cursor.is_empty() {
            args["cursor"] = json!(req.cursor);
        }
        if !req.direction.is_empty() {
            args["direction"] = json!(req.direction);
        }
        self.transport.call_typed("list_messages", args).await
    }

    /// Append an event to the global inbox (M7 event sourcing). An
    /// empty `event_id` lets the server mint a ULID; the returned
    /// [`Event`] echoes the stored row, including its `status`
    /// (`inbox`).
    pub async fn capture_event(&self, req: CaptureEventRequest) -> Result<Event, Error> {
        let mut args = json!({
            "source": req.source,
            "title": req.title,
            "body": req.body,
        });
        if !req.event_id.is_empty() {
            args["event_id"] = json!(req.event_id);
        }
        if !req.at.is_empty() {
            args["at"] = json!(req.at);
        }
        if !req.mime.is_empty() {
            args["mime"] = json!(req.mime);
        }
        if !req.label_skill.is_empty() {
            args["label_skill"] = json!(req.label_skill);
        }
        if !req.instance_page_id.is_empty() {
            args["instance_page_id"] = json!(req.instance_page_id);
        }
        if !req.provenance.is_null() {
            args["provenance"] = req.provenance;
        }
        self.transport.call_typed("capture_event", args).await
    }

    /// List unprocessed inbox events, newest first. `limit` of 0 means
    /// no limit.
    pub async fn list_inbox(&self, req: ListInboxRequest) -> Result<ListInboxResponse, Error> {
        let mut args = json!({});
        if req.limit > 0 {
            args["limit"] = json!(req.limit);
        }
        self.transport.call_typed("list_inbox", args).await
    }

    /// List an instance's processed event history, oldest first.
    pub async fn list_events(&self, req: ListEventsRequest) -> Result<ListEventsResponse, Error> {
        let mut args = json!({ "instance_page_id": req.instance_page_id });
        if req.limit > 0 {
            args["limit"] = json!(req.limit);
        }
        self.transport.call_typed("list_events", args).await
    }

    /// Bind an inbox event to an instance and mark it processed.
    pub async fn assign_event(
        &self,
        req: AssignEventRequest,
    ) -> Result<AssignEventResponse, Error> {
        self.transport
            .call_typed(
                "assign_event",
                json!({
                    "event_id": req.event_id,
                    "instance_page_id": req.instance_page_id,
                }),
            )
            .await
    }

    /// Open a live CRDT co-editing session on `page_id` over the
    /// WebSocket `/ws` channel, drive it with the caller's `ops`
    /// stream, and yield one [`LiveAck`] per server `op_ack`.
    ///
    /// The first item the returned stream yields is the attach ack for
    /// the session named by every [`LiveOp::session`]; thereafter each
    /// op's base64-encoded `op` bytes are forwarded and the merged
    /// version + post-merge content come back as a [`LiveAck`]. The
    /// session must already be open (call the `open_session` tool first
    /// to learn its id and seed content).
    ///
    /// The gateway must have a CRDT backend wired or the WS upgrade is
    /// refused; that surfaces as [`Error::LiveSession`].
    pub async fn live_session<S>(
        &self,
        ops: S,
    ) -> Result<impl futures_util::Stream<Item = Result<LiveAck, Error>>, Error>
    where
        S: futures_util::Stream<Item = LiveOp> + Send + 'static,
    {
        self.transport.live_session(ops).await
    }

    /// Low-level escape hatch: call an arbitrary MCP tool and get the
    /// raw `result` JSON value back. Public so a downstream test can
    /// exercise a tool this façade doesn't yet wrap.
    pub async fn call_raw(&self, tool: &str, arguments: Value) -> Result<Value, Error> {
        self.transport.call(tool, arguments).await
    }

    /// Upload a document for ingestion via `POST /ingest/upload`: deposit
    /// the inline bytes into the tenant inbox (content-addressed) and run
    /// the same document-ingest path as the `/ingest` webhook. The MIME
    /// `content_type` resolves the handling `document`-backend skill, or
    /// pass `skill` to pin a specific one (e.g. a per-collection skill
    /// when several accept the same MIME). Returns the raw ingest outcome
    /// JSON (`status`, `page_id`, `chunk_count`, …).
    ///
    /// This is a plain HTTP endpoint, not an MCP tool — the SPA can't
    /// deposit a content-addressed blob itself, so the same intake is
    /// exposed here for the CLI and BFF.
    pub async fn ingest_upload(
        &self,
        content_type: &str,
        bytes: &[u8],
        title: Option<String>,
        skill: Option<String>,
    ) -> Result<Value, Error> {
        use base64::Engine as _;
        let bytes_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut body = json!({ "content_type": content_type, "bytes_b64": bytes_b64 });
        if let Some(t) = title {
            body["title"] = json!(t);
        }
        if let Some(s) = skill {
            body["skill"] = json!(s);
        }
        self.transport.post_json("/ingest/upload", body).await
    }
}
