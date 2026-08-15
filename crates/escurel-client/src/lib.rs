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
    EmbeddingReloadRequest, EmbeddingReloadResponse, ExportPackRequest, HealthRequest,
    HealthResponse, PackManifest, QuotaGetRequest, QuotaGetResponse, RebuildProgress,
    RebuildRequest, TenantCreateRequest, TenantCreateResponse, TenantDeleteRequest,
    TenantDeleteResponse, TenantExportRequest, TenantGetRequest, TenantGetResponse,
    TenantImportResponse, TenantListRequest, TenantListResponse, TenantUpdateRequest,
    TenantUpdateResponse,
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
    CaptureEventRequest, ChatMessage, DeletePageRequest, DeletePageResponse, Edge, Event,
    ExpandBlock, ExpandRequest, ExpandResponse, InstanceInfo, ListEventsRequest,
    ListEventsResponse, ListInboxRequest, ListInboxResponse, ListInstancesRequest,
    ListInstancesResponse, ListMessagesRequest, ListMessagesResponse, ListSkillsRequest,
    ListSkillsResponse, LiveAck, LiveOp, MovePageRequest, MovePageResponse, NeighboursRequest,
    NeighboursResponse, PageRef, ProvenanceAncestryRequest, ProvenanceAncestryResponse,
    ProvenancePathRequest, ProvenancePathResponse, ProvenanceReportRequest,
    ProvenanceReportResponse, PurgePageRequest, PurgePageResponse, QueryInstanceRequest,
    QueryInstanceResponse, ResolveRequest, ResolveResponse, SearchHit, SearchRequest,
    SearchResponse, Skill, StoredQueryColumn, TenantSpec, UpdatePageRequest, UpdatePageResponse,
    ValidateRequest, ValidateResponse, ValidationIssue, WikilinkParsed,
};
// #247 tenant lifecycle/quota/embedding sub-types.
pub use escurel_types::{EmbeddingSpec, QuotaOverride, TenantStatus};
// Typed shapes for the previously call_raw-only agent tools: the blob /
// CRDT-history reads, the remote write-back, and the HTTP session trio.
pub use escurel_types::{
    ApplyOpRequest, ApplyOpResponse, BlobInfo, CloseSessionRequest, CloseSessionResponse,
    FetchBlobRequest, FetchBlobResponse, ListOpAuthorsRequest, ListOpAuthorsResponse,
    ListSnapshotsRequest, ListSnapshotsResponse, OpAuthor, OpenSessionRequest, OpenSessionResponse,
    WriteInstanceRequest, WriteInstanceResponse,
};
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
        let mut args = json!({ "wikilink": req.wikilink });
        if !req.scenario.is_empty() {
            args["scenario"] = json!(req.scenario);
        }
        self.transport.call_typed("resolve", args).await
    }

    /// Fetch a page's frontmatter, body, and outbound wikilinks.
    ///
    /// A plain read (no `as_of`/`scenario`) also returns the guard pair
    /// for the read→hash→guarded-write loop: `content_sha256` (always,
    /// on a server ≥ #408) and `version` (live-CRDT gateways).
    pub async fn expand(&self, req: ExpandRequest) -> Result<ExpandResponse, Error> {
        let mut args = json!({ "page_id": req.page_id });
        if !req.anchor.is_empty() {
            args["anchor"] = json!(req.anchor);
        }
        if !req.version.is_empty() {
            args["version"] = json!(req.version);
        }
        // Time-travel cut and scenario overlay — optional-with-meaning:
        // omitting them silently read the current base state.
        if !req.as_of.is_empty() {
            args["as_of"] = json!(req.as_of);
        }
        if !req.scenario.is_empty() {
            args["scenario"] = json!(req.scenario);
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
        if !req.as_of.is_empty() {
            args["as_of"] = json!(req.as_of);
        }
        if !req.scenario.is_empty() {
            args["scenario"] = json!(req.scenario);
        }
        self.transport.call_typed("neighbours", args).await
    }

    /// Bounded multi-hop provenance ancestry (ADR-0010).
    pub async fn provenance_ancestry(
        &self,
        req: ProvenanceAncestryRequest,
    ) -> Result<ProvenanceAncestryResponse, Error> {
        let mut args = json!({ "page_id": req.page_id });
        if !req.direction.is_empty() {
            args["direction"] = json!(req.direction);
        }
        if !req.relations.is_empty() {
            args["relations"] = json!(req.relations);
        }
        if req.max_hops > 0 {
            args["max_hops"] = json!(req.max_hops);
        }
        if !req.as_of.is_empty() {
            args["as_of"] = json!(req.as_of);
        }
        self.transport.call_typed("provenance_ancestry", args).await
    }

    /// Corpus-wide provenance report (ADR-0010): `kind: "drift"` or
    /// `kind: "abandoned"`, consolidated from the old
    /// `expectation_drift` / `abandoned_paths` tools.
    pub async fn provenance_report(
        &self,
        req: ProvenanceReportRequest,
    ) -> Result<ProvenanceReportResponse, Error> {
        let mut args = json!({ "kind": req.kind });
        if !req.skill.is_empty() {
            args["skill"] = json!(req.skill);
        }
        self.transport.call_typed("provenance_report", args).await
    }

    /// Shortest provenance path / reachability between two pages —
    /// `provenance_ancestry`'s path mode (the old `provenance_path`
    /// tool, consolidated; `from_page` binds via the server alias).
    pub async fn provenance_path(
        &self,
        req: ProvenancePathRequest,
    ) -> Result<ProvenancePathResponse, Error> {
        let mut args = json!({ "from_page": req.from_page, "to_page": req.to_page });
        if !req.direction.is_empty() {
            args["direction"] = json!(req.direction);
        }
        if !req.relations.is_empty() {
            args["relations"] = json!(req.relations);
        }
        if req.max_hops > 0 {
            args["max_hops"] = json!(req.max_hops);
        }
        self.transport.call_typed("provenance_ancestry", args).await
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
        if !req.as_of.is_empty() {
            args["as_of"] = json!(req.as_of);
        }
        if !req.scenario.is_empty() {
            args["scenario"] = json!(req.scenario);
        }
        // Resume cursor from the previous response's `next_cursor`.
        // Only an absent/null `next_cursor` means done — a string always
        // means more rows (ACL filtering may shorten a page below
        // `limit` with rows still to come).
        if !req.cursor.is_empty() {
            args["cursor"] = json!(req.cursor);
        }
        self.transport.call_typed("list_instances", args).await
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
    ///
    /// The optional guards on [`UpdatePageRequest`] make this the write
    /// half of the read→hash→guarded-write loop: `base_version` (#246)
    /// is the optimistic-concurrency CAS with CRDT auto-merge,
    /// `require_exact_base` makes it strict (approvals), and
    /// `base_sha256` (#354) is the content-hash CAS that works on every
    /// gateway — `Some("")` approves a create. All default to absent =
    /// unguarded, so existing callers are unchanged.
    pub async fn update_page(&self, req: UpdatePageRequest) -> Result<UpdatePageResponse, Error> {
        let mut args = json!({ "page_id": req.page_id, "content": req.content });
        if let Some(v) = &req.base_version {
            args["base_version"] = json!(v);
        }
        if req.require_exact_base {
            args["require_exact_base"] = json!(true);
        }
        // `Some("")` is the approve-create sentinel and MUST reach the
        // wire as the explicit empty string; only `None` is omitted.
        if let Some(h) = &req.base_sha256 {
            args["base_sha256"] = json!(h);
        }
        if let Some(p) = &req.provenance {
            args["provenance"] = p.clone();
        }
        self.transport.call_typed("update_page", args).await
    }

    /// Soft-delete (archive) a markdown page (#300). Retracts it from
    /// discovery while retaining the canonical markdown for audit. An empty
    /// `base_version` skips the optimistic-concurrency guard.
    pub async fn delete_page(&self, req: DeletePageRequest) -> Result<DeletePageResponse, Error> {
        let mut args = json!({ "page_id": req.page_id });
        if !req.base_version.is_empty() {
            args["base_version"] = json!(req.base_version);
        }
        self.transport.call_typed("delete_page", args).await
    }

    /// Permanently remove an already-archived page, finishing what
    /// [`Self::delete_page`] started. Refuses a live page.
    pub async fn purge_page(&self, req: PurgePageRequest) -> Result<PurgePageResponse, Error> {
        self.transport
            .call_typed("purge_page", json!({ "page_id": req.page_id }))
            .await
    }

    /// Move a page to a new `page_id`, leaving nothing at the old one.
    ///
    /// Use this to restructure ids; use [`Self::delete_page`] to retract
    /// knowledge. A delete retains the old markdown as an audit record, which
    /// is right for a retraction and pure noise for a move.
    pub async fn move_page(&self, req: MovePageRequest) -> Result<MovePageResponse, Error> {
        self.transport
            .call_typed("move_page", json!({ "from": req.from, "to": req.to }))
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
        if !req.cursor.is_empty() {
            args["cursor"] = json!(req.cursor);
        }
        self.transport.call_typed("list_inbox", args).await
    }

    /// List an instance's processed event history, oldest first — or, with
    /// [`ListEventsRequest::event_id`] set, look one event up by id.
    pub async fn list_events(&self, req: ListEventsRequest) -> Result<ListEventsResponse, Error> {
        // Send one shape or the other, never both: `event_id` asks WHERE an
        // event went, which makes `instance_page_id` meaningless, and the
        // server should not have to guess which the caller meant.
        let mut args = match &req.event_id {
            Some(event_id) => json!({ "event_id": event_id }),
            None => json!({ "instance_page_id": req.instance_page_id }),
        };
        if req.limit > 0 {
            args["limit"] = json!(req.limit);
        }
        if !req.cursor.is_empty() {
            args["cursor"] = json!(req.cursor);
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

    /// Fetch the original retained file bytes of a `document`-backed
    /// instance (base64 + declared content type) for a faithful client
    /// preview. `blob: None` is ONE indistinguishable answer for an
    /// absent page, a non-document page, and a page the caller may not
    /// read (no existence oracle). The transfer is server-capped.
    pub async fn fetch_blob(&self, req: FetchBlobRequest) -> Result<FetchBlobResponse, Error> {
        self.transport
            .call_typed("fetch_blob", json!({ "page_id": req.page_id }))
            .await
    }

    /// List the `taken_at` timestamps of a page's CRDT snapshot history,
    /// oldest first — the discrete state-over-time points
    /// `expand(as_of=T)` can replay.
    pub async fn list_snapshots(
        &self,
        req: ListSnapshotsRequest,
    ) -> Result<ListSnapshotsResponse, Error> {
        self.transport
            .call_typed("list_snapshots", json!({ "page_id": req.page_id }))
            .await
    }

    /// Who wrote each live-editing (CRDT) op on a page, oldest first —
    /// the read side of write attribution (#357). The `principal` is the
    /// gateway-verified caller, never the Loro peer id in the payload.
    /// A page the caller may not read reports an empty history.
    pub async fn list_op_authors(
        &self,
        req: ListOpAuthorsRequest,
    ) -> Result<ListOpAuthorsResponse, Error> {
        self.transport
            .call_typed("list_op_authors", json!({ "page_id": req.page_id }))
            .await
    }

    /// Write-back to a remote (openapi/mcp) instance's upstream, gated by
    /// the target instance's `acl.update` (fail-closed). Returns the
    /// re-projected upstream state after the write. A binding with no
    /// `write` op is refused (`backend_read_only`).
    pub async fn write_instance(
        &self,
        req: WriteInstanceRequest,
    ) -> Result<WriteInstanceResponse, Error> {
        let payload = if req.payload.is_null() {
            json!({})
        } else {
            req.payload
        };
        self.transport
            .call_typed(
                "write_instance",
                json!({ "ref": req.instance_ref, "payload": payload }),
            )
            .await
    }

    /// Open a live CRDT co-editing session on a page over HTTP. Returns
    /// the session id every subsequent [`Self::apply_op`] /
    /// [`Self::close_session`] (or WS `hello`) names, the page's head
    /// version at open time, and the advisory WS upgrade path. Requires
    /// a gateway with a CRDT backend.
    pub async fn open_session(
        &self,
        req: OpenSessionRequest,
    ) -> Result<OpenSessionResponse, Error> {
        self.transport
            .call_typed("open_session", json!({ "page_id": req.page_id }))
            .await
    }

    /// Apply one base64-encoded Loro op blob to an open session over
    /// HTTP (the polling alternative to the WS channel of
    /// [`Self::live_session`]). The op author is always the verified
    /// token subject — there is no way to name one (#357).
    pub async fn apply_op(&self, req: ApplyOpRequest) -> Result<ApplyOpResponse, Error> {
        self.transport
            .call_typed("apply_op", json!({ "session": req.session, "op": req.op }))
            .await
    }

    /// Close a live session. `commit: true` (the default, also of
    /// [`CloseSessionRequest::default`]) snapshots the merged doc AND
    /// writes the merged body through to the indexer, so reads and
    /// search observe it; `commit: false` discards.
    pub async fn close_session(
        &self,
        req: CloseSessionRequest,
    ) -> Result<CloseSessionResponse, Error> {
        self.transport
            .call_typed(
                "close_session",
                json!({ "session": req.session, "commit": req.commit }),
            )
            .await
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
