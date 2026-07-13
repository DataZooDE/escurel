//! Wire-contract tests for `escurel-types`.
//!
//! Two kinds of checks:
//!
//! 1. **Wire-shape pinning** — deserialize a hand-written JSON literal
//!    that mirrors the MCP wire (copied from the `escurel-server`
//!    `mcp.rs` `json!` builders and the `escurel-test-support`
//!    `decode_*` helpers) into the typed struct, asserting the
//!    expected value, then re-serialize and assert the wire keys.
//! 2. **Serde round-trip** — `from_value(to_value(x)) == x` for a
//!    sampling of every module's types.

use escurel_types::*;
use serde_json::json;

// ── 1. Wire-shape pinning (the divergences) ───────────────────────

#[test]
fn search_hit_wire_shape() {
    // Mirrors tool_search's per-hit json! builder + decode_search.
    let wire = json!({
        "page_id": "instances/customer/acme",
        "slug": "acme",
        "skill": "customer",
        "page_type": "instance",
        "anchor": "overview",
        "snippet": "Acme is a customer",
        "score": 0.87,
        "similarity": 0.0,
        "frontmatter_excerpt": { "tier": "gold" }
    });
    let hit: SearchHit = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(hit.page_id, "instances/customer/acme");
    assert_eq!(hit.page_type, "instance");
    assert_eq!(hit.score, 0.87);
    // frontmatter_excerpt is a real JSON object, not a string
    assert_eq!(hit.frontmatter_excerpt["tier"], "gold");
    // round-trips to the same wire object
    assert_eq!(serde_json::to_value(&hit).unwrap(), wire);
}

#[test]
fn search_response_carries_granularity() {
    let wire = json!({ "hits": [], "granularity": "block" });
    let resp: SearchResponse = serde_json::from_value(wire).unwrap();
    assert_eq!(resp.granularity, "block");
    assert!(resp.hits.is_empty());
    let back = serde_json::to_value(&resp).unwrap();
    assert_eq!(back["granularity"], "block");
}

#[test]
fn expand_response_frontmatter_is_object() {
    // Mirrors tool_expand's Some(e) builder + decode_expand.
    let wire = json!({
        "page": {
            "page_id": "instances/customer/acme",
            "slug": "acme",
            "skill": "customer",
            "page_type": "instance"
        },
        "frontmatter": { "tier": "gold", "at": "2026-01-01" },
        "body": "# Acme",
        "blocks": [ { "anchor": "overview", "content": "hello" } ],
        "wikilinks_out": [
            { "skill": "person", "id": "jane", "anchor": "", "version": "", "alias": "" }
        ]
    });
    let resp: ExpandResponse = serde_json::from_value(wire).unwrap();
    assert_eq!(resp.page.as_ref().unwrap().slug, "acme");
    assert_eq!(resp.blocks.len(), 1);
    assert_eq!(resp.blocks[0].anchor, "overview");
    assert_eq!(resp.wikilinks_out[0].id, "jane");
    // frontmatter is a real JSON object, not a string
    assert_eq!(resp.frontmatter["tier"], "gold");
    let back = serde_json::to_value(&resp).unwrap();
    assert!(back["frontmatter"].is_object());
    assert_eq!(back["body"], "# Acme");
}

#[test]
fn expand_response_null_page() {
    // tool_expand returns `{ "page": null }` when the page is absent.
    let wire = json!({ "page": null });
    let resp: ExpandResponse = serde_json::from_value(wire).unwrap();
    assert!(resp.page.is_none());
    let back = serde_json::to_value(&resp).unwrap();
    // page omitted on serialize (skip_serializing_if None)
    assert!(back.get("page").is_none());
}

#[test]
fn resolve_response_wire_shape() {
    // Mirrors tool_resolve + decode_resolve. Note: wire key is
    // `exists`, not `found`.
    let wire = json!({
        "parsed": { "skill": "customer", "id": "acme", "anchor": "", "version": "", "alias": "" },
        "page": {
            "page_id": "instances/customer/acme",
            "slug": "acme",
            "skill": "customer",
            "page_type": "instance"
        },
        "exists": true
    });
    let resp: ResolveResponse = serde_json::from_value(wire).unwrap();
    assert!(resp.exists);
    assert_eq!(
        resp.page.as_ref().unwrap().page_id,
        "instances/customer/acme"
    );
    assert_eq!(resp.parsed.as_ref().unwrap().id, "acme");
    let back = serde_json::to_value(&resp).unwrap();
    assert_eq!(back["exists"], true);

    // not-found: page null/absent
    let wire_none = json!({
        "parsed": { "skill": "", "id": "ghost", "anchor": "", "version": "", "alias": "" },
        "page": null,
        "exists": false
    });
    let resp_none: ResolveResponse = serde_json::from_value(wire_none).unwrap();
    assert!(!resp_none.exists);
    assert!(resp_none.page.is_none());
    assert!(
        serde_json::to_value(&resp_none)
            .unwrap()
            .get("page")
            .is_none()
    );
}

#[test]
fn instance_info_frontmatter_is_object() {
    // Mirrors tool_list_instances + decode_list_instances.
    let wire = json!({
        "page_id": "instances/customer/acme",
        "skill": "customer",
        "frontmatter": { "tier": "gold" },
        "at": "2026-01-01"
    });
    let info: InstanceInfo = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(info.page_id, "instances/customer/acme");
    assert_eq!(info.frontmatter["tier"], "gold");
    assert_eq!(serde_json::to_value(&info).unwrap(), wire);
}

#[test]
fn list_instances_request_skill_id_rename() {
    // The MCP wire takes the skill under `skill_id`, not `skill`.
    let wire = json!({ "skill_id": "customer", "limit": 5 });
    let req: ListInstancesRequest = serde_json::from_value(wire).unwrap();
    assert_eq!(req.skill, "customer");
    assert_eq!(req.limit, 5);
    let back = serde_json::to_value(&req).unwrap();
    assert_eq!(back["skill_id"], "customer");
    assert!(back.get("skill").is_none());
}

#[test]
fn skill_wire_shape() {
    // Mirrors tool_list_skills + decode_list_skills. `backend` +
    // `capabilities` are additive (REQ-BK-02): markdown-backed skills carry
    // `{"kind":"markdown"}` and the markdown capability descriptor.
    // `layer` is additive too (REQ-LAYER-04): `"overlay"` for a
    // tenant-authored skill, `"base@<pack>@<version>"` for one imported
    // from a subscribed pack.
    let wire = json!({
        "id": "customer",
        "description": "A customer record",
        "required_frontmatter": ["tier"],
        "optional_frontmatter": ["region"],
        "is_event_typed": false,
        "visibility": "public",
        "owner_field": null,
        "backend": { "kind": "markdown" },
        "capabilities": {
            "writable": true,
            "granularity": "block",
            "search": "hybrid",
            "supports_crdt": true
        },
        "layer": "overlay"
    });
    let skill: Skill = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(skill.id, "customer");
    assert_eq!(skill.required_frontmatter, vec!["tier"]);
    assert_eq!(skill.visibility, "public");
    assert_eq!(skill.owner_field, None);
    assert_eq!(skill.backend.kind, "markdown");
    assert!(skill.capabilities.writable);
    assert_eq!(skill.layer, "overlay");
    assert_eq!(serde_json::to_value(&skill).unwrap(), wire);
}

#[test]
fn skill_layer_defaults_to_overlay_on_old_servers() {
    // An old server that doesn't emit `layer` must parse to the overlay
    // default — pre-layer skills are tenant-authored and editable.
    let wire = json!({
        "id": "customer",
        "description": "A customer record",
    });
    let skill: Skill = serde_json::from_value(wire).unwrap();
    assert_eq!(skill.layer, "overlay");
}

#[test]
fn event_provenance_is_value() {
    // Mirrors event_to_json: wire key `provenance` carries a JSON value.
    let wire = json!({
        "event_id": "e1",
        "at": "2026-05-30T00:00:00Z",
        "source": "gmail",
        "mime": "message/rfc822",
        "label_skill": "email",
        "instance_page_id": "",
        "status": "inbox",
        "title": "Hello",
        "body": "world",
        "provenance": { "thread": "t1" }
    });
    let ev: Event = serde_json::from_value(wire).unwrap();
    assert_eq!(ev.source, "gmail");
    assert_eq!(ev.status, "inbox");
    assert_eq!(ev.provenance["thread"], "t1");
    let back = serde_json::to_value(&ev).unwrap();
    assert!(back["provenance"].is_object());
}

#[test]
fn assign_event_response_has_status() {
    // tool_assign_event emits {event_id, instance_page_id, status}.
    let wire = json!({
        "event_id": "e1",
        "instance_page_id": "instances/customer/acme",
        "status": "processed"
    });
    let resp: AssignEventResponse = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(resp.status, "processed");
    assert_eq!(serde_json::to_value(&resp).unwrap(), wire);
}

#[test]
fn run_stored_query_response_rows_and_column_type() {
    // tool_run_stored_query: rows is an array, schema columns carry `type`.
    let wire = json!({
        "rows": [ { "title": "Acme", "score": 0.9 } ],
        "schema": [
            { "name": "title", "type": "VARCHAR" },
            { "name": "score", "type": "DOUBLE" }
        ]
    });
    let resp: RunStoredQueryResponse = serde_json::from_value(wire).unwrap();
    assert_eq!(resp.schema[0].type_name, "VARCHAR");
    assert!(resp.rows.is_array());
    assert_eq!(resp.rows[0]["title"], "Acme");
    let back = serde_json::to_value(&resp).unwrap();
    assert_eq!(back["schema"][0]["type"], "VARCHAR");
    assert!(back["schema"][0].get("type_name").is_none());
    assert!(back["rows"].is_array());
}

#[test]
fn run_stored_query_request_params_is_object() {
    let wire = json!({ "query_id": "by_tier", "params": { "tier": "gold" } });
    let req: RunStoredQueryRequest = serde_json::from_value(wire).unwrap();
    assert_eq!(req.params["tier"], "gold");
    let back = serde_json::to_value(&req).unwrap();
    assert!(back["params"].is_object());
    assert!(back.get("params_json").is_none());
}

#[test]
fn capture_event_request_provenance_is_object() {
    let wire = json!({
        "source": "gmail",
        "mime": "message/rfc822",
        "label_skill": "email",
        "title": "Hello",
        "body": "world",
        "provenance": { "thread": "t1" }
    });
    let req: CaptureEventRequest = serde_json::from_value(wire).unwrap();
    assert_eq!(req.provenance["thread"], "t1");
    assert!(serde_json::to_value(&req).unwrap()["provenance"].is_object());
}

#[test]
fn chat_message_optional_metadata() {
    // chat_message_to_json: metadata only present when set.
    let wire = json!({
        "chat_group_id": "g1",
        "msg_id": "m1",
        "ts": "2026-05-30T10:00:00Z",
        "role": "user",
        "content": "hi",
        "embedded": true,
        "metadata": { "k": "v" }
    });
    let m: ChatMessage = serde_json::from_value(wire).unwrap();
    assert_eq!(m.role, "user");
    assert_eq!(m.metadata.as_ref().unwrap()["k"], "v");
    assert!(m.author.is_none());
    let back = serde_json::to_value(&m).unwrap();
    assert!(back.get("author").is_none());
    assert!(back["metadata"].is_object());
}

#[test]
fn admin_lane_blob_response_bytes_base64_wire() {
    // The live MCP `admin_lane_blob` tool emits the payload as a
    // base64 *string* under `bytes_base64` (not a raw byte array).
    let wire = json!({ "bytes_base64": "AQID", "content_type": "text/markdown" });
    let resp: AdminLaneBlobResponse = serde_json::from_value(wire).unwrap();
    assert_eq!(resp.bytes_base64, "AQID");
    let back = serde_json::to_value(&resp).unwrap();
    assert_eq!(back["bytes_base64"], json!("AQID"));
    assert!(back.get("bytes").is_none());
    assert!(back.get("data").is_none());
}

#[test]
fn missing_keys_default() {
    // MCP wire omits empty fields; deserialize from a sparse object.
    let hit: SearchHit = serde_json::from_value(json!({ "slug": "x" })).unwrap();
    assert_eq!(hit.slug, "x");
    assert_eq!(hit.score, 0.0);
    assert_eq!(hit.page_id, "");
    assert!(hit.frontmatter_excerpt.is_null());
}

#[test]
fn search_hit_tolerates_null_anchor() {
    // A `sql_view` candidate hit has no block anchor — the live gateway
    // emits an explicit `"anchor": null` (page-grain hit). The typed
    // client must decode it (`null_as_default`), not fail with
    // "invalid type: null, expected a string" — otherwise every
    // sql_view-bearing result set breaks the typed `search()`.
    let wire = json!({
        "page_id": "markdown/instances/customers/eu.md",
        "slug": "eu",
        "skill": "customers",
        "page_type": "instance",
        "anchor": null,
        "snippet": "matched 2 rows",
        "score": 0.5,
        "similarity": 0.0,
        "frontmatter_excerpt": { "backend_ref": { "kind": "sql_view" } }
    });
    let hit: SearchHit = serde_json::from_value(wire).expect("null anchor must decode");
    assert_eq!(hit.anchor, "");
    assert_eq!(hit.skill, "customers");
}

// ── 2. Serde round-trips per module ───────────────────────────────

fn rt<T>(x: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let v = serde_json::to_value(&x).unwrap();
    let back: T = serde_json::from_value(v).unwrap();
    assert_eq!(x, back);
}

#[test]
fn roundtrip_core() {
    rt(PageRef {
        page_id: "p".into(),
        slug: "s".into(),
        skill: "sk".into(),
        page_type: "instance".into(),
    });
    rt(WikilinkParsed {
        skill: "sk".into(),
        id: "i".into(),
        anchor: "a".into(),
        version: "v".into(),
        alias: "al".into(),
    });
}

#[test]
fn roundtrip_agent() {
    rt(SearchResponse {
        hits: vec![SearchHit {
            slug: "s".into(),
            score: 1.5,
            frontmatter_excerpt: json!({ "k": "v" }),
            ..Default::default()
        }],
        granularity: "page".into(),
    });
    rt(ExpandResponse {
        page: Some(PageRef::default()),
        frontmatter: json!({ "k": "v" }),
        body: "b".into(),
        blocks: vec![ExpandBlock {
            anchor: "a".into(),
            content: "c".into(),
        }],
        wikilinks_out: vec![WikilinkParsed::default()],
    });
    rt(ResolveResponse {
        parsed: Some(WikilinkParsed::default()),
        page: None,
        exists: false,
    });
    rt(NeighboursResponse {
        edges: vec![Edge {
            src_page: "a".into(),
            dst_page: "b".into(),
            link_skill: "owns".into(),
            link_version: "".into(),
            dst_anchor: "".into(),
        }],
    });
    rt(RunStoredQueryResponse {
        rows: json!([{ "n": 1 }]),
        schema: vec![StoredQueryColumn {
            name: "n".into(),
            type_name: "INTEGER".into(),
        }],
    });
    rt(ValidateResponse {
        ok: false,
        issues: vec![ValidationIssue {
            severity: "error".into(),
            code: "c".into(),
            location: "frontmatter".into(),
            message: "m".into(),
            suggestion: Some("fix it".into()),
        }],
    });
    rt(UpdatePageResponse {
        ok: true,
        issues: vec![],
        new_version: "v1".into(),
    });
    rt(ListSkillsResponse {
        skills: vec![Skill {
            id: "s".into(),
            required_frontmatter: vec!["a".into()],
            ..Default::default()
        }],
    });
    rt(ListInstancesResponse {
        instances: vec![InstanceInfo {
            page_id: "p".into(),
            frontmatter: json!({}),
            ..Default::default()
        }],
        next_cursor: None,
    });
}

#[test]
fn roundtrip_chat() {
    rt(ChatMessage {
        chat_group_id: "g".into(),
        msg_id: "m".into(),
        ts: "now".into(),
        role: "user".into(),
        content: "hi".into(),
        embedded: true,
        author: Some("jr".into()),
        metadata: Some(json!({ "x": 1 })),
    });
    rt(AppendMessageResponse {
        msg_id: "m".into(),
        ts: "now".into(),
    });
    rt(ListMessagesResponse {
        messages: vec![ChatMessage::default()],
        next_cursor: Some("c".into()),
    });
}

#[test]
fn roundtrip_events() {
    rt(Event {
        event_id: "e".into(),
        provenance: json!({ "x": 1 }),
        status: "inbox".into(),
        ..Default::default()
    });
    rt(ListInboxResponse {
        events: vec![Event::default()],
    });
    rt(AssignEventResponse {
        event_id: "e".into(),
        instance_page_id: "p".into(),
        status: "processed".into(),
    });
}

#[test]
fn roundtrip_session() {
    rt(LiveOp {
        session: "s".into(),
        op: vec![1, 2, 3],
    });
    rt(LiveAck {
        session: "s".into(),
        merged_version: "v2".into(),
        content: "merged".into(),
        issues: vec![],
    });
}

#[test]
fn roundtrip_admin() {
    rt(TenantSpec {
        tenant_id: "t".into(),
        display_name: "T".into(),
        ..Default::default()
    });
    rt(TenantCreateResponse {
        spec: Some(TenantSpec::default()),
    });
    rt(TenantListResponse {
        tenants: vec![TenantSpec::default()],
    });
    rt(TenantDeleteResponse { deleted: true });
    rt(TenantExportChunk {
        data: vec![0xde, 0xad],
    });
    rt(TenantImportResponse {
        bytes_imported: 1024,
    });
    rt(AuditResponse {
        markdown_not_in_duckdb: vec!["a.md".into()],
        indexed_but_no_markdown: vec![],
    });
    rt(RebuildProgress {
        done: 2,
        total: 10,
        current_page: "p".into(),
    });
    rt(QuotaGetResponse {
        queries_remaining: 100,
        writes_remaining: 50,
        embeds_remaining: 10,
        concurrent_sessions: 2,
    });
    rt(HealthResponse {
        status: "ok".into(),
        version: "1.0.0".into(),
    });
    rt(AdminListLanesResponse {
        lanes: vec![LaneInfo {
            name: "markdown".into(),
            backend: "fs".into(),
            tenants_present: vec!["acme".into()],
        }],
    });
    rt(AdminLaneKeysResponse {
        keys: vec![LaneKey {
            key: "k".into(),
            size_bytes: 99,
        }],
    });
    rt(AdminLaneBlobResponse {
        bytes_base64: "AQI=".to_owned(),
        content_type: "text/markdown".into(),
    });
    rt(DeleteChatHistoryResponse { deleted: 4 });
    rt(CompactProgress {
        ops_compacted: 3,
        bytes_reclaimed: 4096,
    });
}
