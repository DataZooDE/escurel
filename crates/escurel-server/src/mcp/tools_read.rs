//! Agent-facing read tools: discovery, resolution, expansion, traversal, provenance, search and stored queries.
//!
//! Split out of `mcp.rs` (R1 of
//! `docs/notes/complexity-reduction-plan.md`). These are the tools that never mutate state, so they share no write
//! lock, no event emission and no ACL-on-write path with the group below.

use super::backend_view::BackendView;
use super::*;

// --- per-tool handlers -----------------------------------------

/// The Tier-1 catalogue, **scoped to the caller** (#374).
///
/// Two things change for a non-admin caller:
///
/// 1. a skill whose declared `acl.read` excludes them is filtered out
///    entirely — denial as absence, like every sibling read verb, and what
///    lets a downstream client trust the catalogue rather than re-filter it
///    (the D27/D28 failure server-side scoping exists to prevent);
/// 2. the `acl` block is **not projected**. Group names are authorisation
///    metadata, not schema: in a shared tenant they are named per
///    engagement (`engagement-hoffmann`), so shipping the grant list to
///    every token holder discloses the customer roster and the
///    authorisation topology. A client cannot act on a grant it does not
///    hold, so nothing in the documented flow needs it. `visibility` /
///    `owner_field` stay — they describe how instances behave and name no
///    group.
pub(super) async fn tool_list_skills(
    indexer: &Indexer,
    caller: AclCaller<'_>,
) -> Result<Value, JsonRpcError> {
    let all = indexer
        .list_skills()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_skills: {e}")))?;
    let is_admin = caller.is_admin;
    let skills = indexer
        .filter_readable_skills(&caller, all)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_skills: {e}")))?;
    let resp = ListSkillsResponse {
        skills: skills
            .into_iter()
            .map(|s| TypesSkill {
                id: s.id,
                description: s.description,
                required_frontmatter: s.required_frontmatter,
                optional_frontmatter: s.optional_frontmatter,
                is_event_typed: s.is_event_typed,
                visibility: match s.visibility {
                    Visibility::Public => "public".to_string(),
                    Visibility::Owner => "owner".to_string(),
                },
                owner_field: s.owner_field,
                // Admin-only. `None` is omitted from the wire, so a
                // non-admin row is byte-identical to one for a skill that
                // declares no block at all — the redaction is not an
                // existence oracle either.
                acl: s.acl.filter(|_| is_admin).map(|a| TypesSkillAcl {
                    read: a.read,
                    create: a.create,
                    update: a.update,
                    delete: a.delete,
                }),
                backend: TypesSkillBackend {
                    kind: s.backend.kind.as_str().to_string(),
                },
                capabilities: {
                    let c = Capabilities::for_kind(s.backend.kind);
                    TypesSkillCapabilities {
                        writable: c.writable,
                        granularity: c.granularity.as_str().to_string(),
                        search: c.search.as_str().to_string(),
                        supports_crdt: c.supports_crdt,
                    }
                },
                layer: s.layer.unwrap_or_else(|| "overlay".to_owned()),
                shadows: s.shadows,
                // `None` — absent or unrecognised — stays absent on the wire.
                // It must not be defaulted to a policy here: the only value a
                // consumer may act on permissively is an explicit `auto`.
                autonomy: s.autonomy.map(|a| a.as_str().to_owned()),
                // Empty for every skill that declares no `params:`, and an
                // empty vec is omitted from the wire — so those rows stay
                // byte-identical to what they were before CR-7.
                params: s
                    .params
                    .into_iter()
                    .map(|p| TypesSkillParam {
                        name: p.name,
                        kind: p.kind.as_str().to_owned(),
                        required: p.required,
                        label: p.label,
                        description: p.description,
                    })
                    .collect(),
            })
            .collect(),
    };
    to_value(resp)
}

#[derive(Deserialize)]
pub(super) struct ListInstancesArgs {
    #[serde(alias = "skill")]
    skill_id: String,
    /// Resume cursor from a previous page's `next_cursor`.
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    order_by: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Optional single frontmatter equality filter, e.g.
    /// `{"frontmatter_key": "source", "frontmatter_value": "gmail"}`
    /// for the source-inbox view. Both must be present to apply.
    #[serde(default)]
    frontmatter_key: Option<String>,
    #[serde(default)]
    frontmatter_value: Option<String>,
    /// RFC 3339 time-travel cut; instances born after it are excluded
    /// (untimed instances always remain).
    #[serde(default)]
    as_of: Option<String>,
    /// Scenario overlay; null/absent = base only, else base ∪ overlay
    /// with the overlay winning per slug.
    #[serde(default)]
    scenario: Option<String>,
}

pub(super) async fn tool_list_instances(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListInstancesArgs = parse_args(args, "list_instances")?;
    let order = match a.order_by.as_deref() {
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "at asc" | "at_asc" => Some(OrderDir::Asc),
            "at desc" | "at_desc" => Some(OrderDir::Desc),
            _ => None,
        },
        None => None,
    };
    let filter = match (a.frontmatter_key.as_deref(), a.frontmatter_value.as_deref()) {
        (Some(k), Some(v)) if !k.is_empty() => Some((k, v)),
        _ => None,
    };
    let (out, next_cursor) = indexer
        .list_instances_page(
            &a.skill_id,
            order,
            a.limit.unwrap_or(10_000),
            filter,
            a.as_of.as_deref(),
            a.scenario.as_deref(),
            a.cursor.as_deref(),
        )
        .await
        .map_err(|e| match e {
            escurel_index::IndexerError::InvalidCursor(msg) => {
                JsonRpcError::invalid_params(format!("list_instances: cursor: {msg}"))
            }
            e => JsonRpcError::internal(format!("list_instances: {e}")),
        })?;
    // Deterministic ACL filter: drop owner-private instances the caller
    // does not own (admin bypasses). Enumeration must not leak what a
    // direct read would deny.
    let mut instances = Vec::with_capacity(out.len());
    for i in &out {
        if indexer
            .may_read_instance(&caller, &i.skill, &i.frontmatter)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_instances acl: {e}")))?
        {
            instances.push(json!({
                "page_id": i.page_id,
                "skill": i.skill,
                "frontmatter": i.frontmatter,
                "at": i.at,
            }));
        }
    }
    // `next_cursor` stays PRESENT (null on the last page) for
    // byte-compat with clients written against the always-null era; a
    // string value means more rows — and ONLY null means done (the ACL
    // filter above legitimately shortens pages).
    Ok(json!({
        "instances": instances,
        "next_cursor": next_cursor,
    }))
}

#[derive(Deserialize)]
pub(super) struct ResolveArgs {
    wikilink: String,
    /// Scenario overlay to resolve against; null/absent = base only.
    #[serde(default)]
    scenario: Option<String>,
}

pub(super) async fn tool_resolve(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ResolveArgs = parse_args(args, "resolve")?;
    let mut resolved = indexer
        .resolve(&a.wikilink, a.scenario.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("resolve: {e}")))?;
    // ACL (always on, mirroring the read filters): never disclose the
    // existence / page_id of an owner-private instance the caller cannot
    // read — resolve it to "not found", exactly as `expand` returns null.
    if let Some(p) = &resolved.page
        && p.page_type == PageType::Instance
    {
        let readable = match indexer
            .expand(&p.page_id, None, None)
            .await
            .map_err(|e| JsonRpcError::internal(format!("resolve acl: {e}")))?
        {
            Some(e) => indexer
                .may_read_instance(&caller, &p.skill, &e.frontmatter)
                .await
                .map_err(|e| JsonRpcError::internal(format!("resolve acl: {e}")))?,
            None => true,
        };
        if !readable {
            resolved.page = None;
        }
    }
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
pub(super) struct ExpandArgs {
    page_id: String,
    /// RFC 3339 time-travel cut; the page resolves to null if born after it.
    #[serde(default)]
    as_of: Option<String>,
    /// Scenario overlay to read against; null/absent = base only.
    #[serde(default)]
    scenario: Option<String>,
    /// Return ALL chunks of a document instance instead of the bounded lead
    /// (REQ-DOC-05). For a single-document detail view (relevance heatmap over
    /// the whole text), not the default grounding/preview path.
    #[serde(default)]
    full: bool,
}

pub(super) async fn tool_expand(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ExpandArgs = parse_args(args, "expand")?;
    let out = indexer
        .expand(&a.page_id, a.as_of.as_deref(), a.scenario.as_deref())
        .await
        .map_err(|e| JsonRpcError::internal(format!("expand: {e}")))?;
    // Deterministic ACL: an owner-private instance the caller does not own
    // reads as absent (null) — same shape as a missing page, so existence
    // is not leaked. Skill pages are the public catalogue, never gated.
    if let Some(e) = &out
        && e.page.page_type == PageType::Instance
        && !indexer
            .may_read_instance(&caller, &e.page.skill, &e.frontmatter)
            .await
            .map_err(|err| JsonRpcError::internal(format!("expand acl: {err}")))?
    {
        return Ok(json!({ "page": Value::Null }));
    }
    match out {
        None => Ok(json!({ "page": Value::Null })),
        Some(e) => {
            let e_page_id = e.page.page_id.clone();
            let mut page = json!({
                "page": {
                    "page_id": e.page.page_id,
                    "slug": e.page.slug,
                    "skill": e.page.skill,
                    "page_type": page_type_str(e.page.page_type),
                    // #357 (CR-6): the verified principal behind the page's
                    // most recent write. `null` for a page last written
                    // before the gateway recorded one, and on an `as_of`
                    // read (a CRDT snapshot carries bytes, not an author) —
                    // never a guess.
                    "last_written_by": e.last_written_by,
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
            });
            // The read half of the approve loop (#354/heron#30): publish
            // the hash of the STORED markdown bytes — exactly the value
            // `update_page`'s `base_sha256` guard compares against — so a
            // client can hold "what the drafter saw" without a write-probe
            // or a byte-perfect reconstruction from parsed fields. Only on
            // PLAIN reads: an `as_of`/`scenario` body is not the current
            // stored bytes, and publishing the current hash beside a
            // historical body would invite guarding the wrong thing.
            if a.as_of.is_none()
                && a.scenario.is_none()
                && let Some(stored) = indexer
                    .read_page_markdown(&e_page_id)
                    .await
                    .map_err(|err| JsonRpcError::internal(format!("expand hash: {err}")))?
            {
                use sha2::{Digest, Sha256};
                page["content_sha256"] = json!(format!("{:x}", Sha256::digest(stored.as_bytes())));
            }
            // #246: surface the page's current monotonic version so a client
            // can pass it back as `base_version` on the next `update_page`
            // (the read→edit→write optimistic-concurrency cycle).
            if let Some(backend) = state.crdt_backend.as_ref() {
                let hlc =
                    u64::try_from(backend.max_hlc(&e_page_id).await.unwrap_or(0)).unwrap_or(0);
                page["version"] = json!(Version::from_op_count(hlc).as_str());
            }
            // Shadowed base (REQ-LAYER-03): when a tenant OVERLAY skill page
            // shadows a pack-imported base skill of the same slug, expose the
            // base page + its frontmatter under a namespaced `shadow` object —
            // the same drift-visibility discipline as the sql_view `source`
            // namespace: the overlay wins for display, the base value stays
            // visible, never silently masked.
            if e.page.page_type == PageType::Skill
                && !e_page_id.starts_with(escurel_index::pack::RESERVED_BASE_PREFIX)
                && let Some(slug) = e.page.slug.as_deref()
                && let Some((base_page_id, pin, base_fm)) = indexer
                    .shadowed_base(slug, &e_page_id)
                    .await
                    .map_err(|err| JsonRpcError::internal(format!("expand shadow: {err}")))?
            {
                page["shadow"] = json!({
                    "base_page_id": base_page_id,
                    "pack": pin,
                    "base": base_fm,
                });
            }
            // SQL-view overlay: render a BOUNDED projection beneath the overlay
            // body (REQ-SQL-06), and expose projected source columns under a
            // namespaced `source` object so overlay↔source drift is visible
            // without the overlay value being masked (REQ-OV-02). The overlay
            // (shown first) always wins for display.
            // Classify once; each enrichment below asks the same question of
            // the same answer instead of re-probing `backend_ref`.
            let view = BackendView::of(&e.frontmatter);
            if view == BackendView::SqlView
                && let Some(proj) = sql_view_projection(indexer, &e).await
            {
                page["backend_projection"] = proj;
            }
            // Document overlay: bound the chunks returned (REQ-DOC-05) — never
            // the full document text. With no query in `expand`, return the
            // lead (first K chunks) and flag truncation.
            if view == BackendView::Document {
                // The skill's `lead_chunks` caps the lead returned (REQ-DOC-05);
                // fall back to the server default. The full text lives in the blob.
                const DEFAULT_CHUNK_LEAD: usize = 8;
                let lead_n = indexer
                    .skill_backend(&e.page.skill)
                    .await
                    .ok()
                    .and_then(|b| b.document.and_then(|d| d.lead_chunks))
                    .unwrap_or(DEFAULT_CHUNK_LEAD);
                let total = e.blocks.len();
                // `full` returns every chunk (detail/heatmap view); otherwise
                // bound to the lead (REQ-DOC-05).
                if !a.full
                    && let Some(arr) = page["blocks"].as_array().cloned()
                {
                    let lead: Vec<Value> = arr.into_iter().take(lead_n).collect();
                    page["blocks"] = Value::from(lead);
                }
                page["chunks_total"] = json!(total);
                page["chunks_truncated"] = json!(!a.full && total > lead_n);
            }
            // Remote (proxy) overlay: fetch the LIVE projection from the
            // upstream openapi/mcp endpoint (nothing is materialised in
            // DuckDB). A failure resolves to `{ issue }` — the overlay page is
            // still returned, never a partial/fabricated body.
            if view == BackendView::RemoteProxy {
                page["backend_projection"] = crate::remote_backend::fetch_projection(
                    indexer,
                    &e.page.skill,
                    e.page.slug.as_deref(),
                )
                .await;
            }
            Ok(page)
        }
    }
}

/// Hard cap on a single `fetch_blob` transfer (25 MiB). Original documents are
/// larger than the admin lane cap but still bounded for a browser preview.
pub(super) const FETCH_BLOB_MAX_BYTES: usize = 25 * 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct FetchBlobArgs {
    page_id: String,
}

/// Return the ORIGINAL retained file bytes for a `document`-backed instance —
/// the blob behind `backend_ref.blob_id` — base64-encoded with a sniffed
/// content type, for a faithful client-side preview of the source document.
///
/// ACL mirrors `expand`: an instance the caller may not read resolves to a null
/// blob (existence is not leaked). Non-document pages and missing pages also
/// resolve to null. The transfer is size-capped.
/// Resolve `page_id` to its retained original blob under `caller`'s read
/// ACL. `Ok(None)` is ONE indistinguishable answer for absent, hidden,
/// and no-fetchable-blob pages (no existence oracle). Shared by the
/// `fetch_blob` tool (base64, capped) and `GET /blob/{page_id}` (raw).
pub(super) async fn resolve_readable_blob(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    page_id: &str,
) -> Result<Option<(String, bytes::Bytes)>, String> {
    let out = indexer
        .expand(page_id, None, None)
        .await
        .map_err(|e| format!("blob expand: {e}"))?;
    let Some(e) = out else {
        return Ok(None);
    };
    if e.page.page_type == PageType::Instance
        && !indexer
            .may_read_instance(caller, &e.page.skill, &e.frontmatter)
            .await
            .map_err(|err| format!("blob acl: {err}"))?
    {
        return Ok(None);
    }
    let blob_id_str = e
        .frontmatter
        .get("backend_ref")
        .and_then(|b| b.get("blob_id"))
        .and_then(Value::as_str);
    if !BackendView::of(&e.frontmatter).has_fetchable_blob() || blob_id_str.is_none() {
        return Ok(None);
    }
    let blob_id = escurel_storage::BlobId::parse(blob_id_str.unwrap())
        .ok_or_else(|| "blob: malformed blob_id".to_owned())?;
    let bytes = indexer
        .read_blob(&blob_id)
        .await
        .map_err(|err| format!("blob read: {err}"))?;
    // Prefer the MIME the upload DECLARED (recorded on the overlay since
    // GH #356) over sniffing the bytes: the sniff knows PDF/OOXML/text and
    // answers `application/octet-stream` for everything else — which is
    // exactly the answer a client cannot play a recording from. Overlays
    // written before that field existed fall back to the sniff, so no
    // already-ingested document changes its answer.
    let declared = e
        .frontmatter
        .get("backend_ref")
        .and_then(|b| b.get("content_type"))
        .and_then(Value::as_str)
        .filter(|ct| !ct.is_empty());
    let content_type = declared
        .unwrap_or_else(|| sniff_content_type(&bytes))
        .to_owned();
    Ok(Some((content_type, bytes)))
}

pub(super) async fn tool_fetch_blob(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: FetchBlobArgs = parse_args(args, "fetch_blob")?;
    let Some((content_type, bytes)) = resolve_readable_blob(indexer, &caller, &a.page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("fetch_blob {e}")))?
    else {
        return Ok(json!({ "blob": Value::Null }));
    };
    if bytes.len() > FETCH_BLOB_MAX_BYTES {
        return Err(JsonRpcError::invalid_params(format!(
            "blob is {} bytes, over the {FETCH_BLOB_MAX_BYTES}-byte fetch cap",
            bytes.len()
        )));
    }
    Ok(json!({
        "blob": {
            "page_id": a.page_id,
            "content_type": content_type,
            "size": bytes.len(),
            "bytes_base64": B64.encode(&bytes),
        }
    }))
}

/// Best-effort content-type sniff for a retained blob: PDF, OOXML
/// (docx/pptx/xlsx by their part markers), then UTF-8 text.
pub(super) fn sniff_content_type(bytes: &[u8]) -> &'static str {
    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        needle.len() <= hay.len() && hay.windows(needle.len()).any(|w| w == needle)
    }
    if bytes.starts_with(b"%PDF") {
        return "application/pdf";
    }
    if bytes.starts_with(b"PK\x03\x04") {
        if contains(bytes, b"word/document.xml") {
            return "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
        }
        if contains(bytes, b"ppt/presentation.xml") {
            return "application/vnd.openxmlformats-officedocument.presentationml.presentation";
        }
        if contains(bytes, b"xl/workbook.xml") {
            return "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
        }
        return "application/zip";
    }
    if std::str::from_utf8(bytes).is_ok() {
        return "text/plain";
    }
    "application/octet-stream"
}

/// Bounded rows + projected `source` fields for a SQL-view instance overlay,
/// or `None` when the page is not a `sql_view` instance.
pub(super) async fn sql_view_projection(
    indexer: &Indexer,
    e: &escurel_index::ExpandedPage,
) -> Option<Value> {
    /// Default rows rendered when the skill declares no `projection_limit`.
    const DEFAULT_PROJECTION_LIMIT: usize = 50;
    let backend_ref = e.frontmatter.get("backend_ref")?;
    if backend_ref.get("kind").and_then(Value::as_str) != Some("sql_view") {
        return None;
    }
    let view = backend_ref.get("view").and_then(Value::as_str)?;

    // Fail closed on schema drift (REQ-NF-06): if the view's current schema
    // fingerprint no longer matches the one captured at create time, return
    // an Issue instead of (possibly wrong) rows.
    if let Some(stored) = backend_ref
        .get("source_schema_fingerprint")
        .and_then(Value::as_str)
    {
        match indexer.current_view_fingerprint(view).await {
            Ok(current) if current != stored => {
                return Some(json!({
                    "view": view, "rows": [], "source": {},
                    "issue": { "code": "binding_degraded",
                        "message": "source schema drifted from the stored fingerprint; \
                                    reads fail closed until the binding is re-validated" },
                }));
            }
            Err(e) => {
                return Some(json!({
                    "view": view, "rows": [], "source": {},
                    "issue": { "code": "source_unavailable", "message": e.to_string() },
                }));
            }
            Ok(_) => {}
        }
    }

    // The skill's `projection_limit` caps the rows rendered (REQ-SQL-06); fall
    // back to the server default, and never exceed the policy cap (so the
    // `limit + 1` truncation sentinel can't be silently clamped by the row
    // reader). Fetch one extra row so `truncated` is exact.
    let binding = indexer.skill_backend(&e.page.skill).await.ok();
    let limit = binding
        .as_ref()
        .and_then(|b| b.projection_limit)
        .unwrap_or(DEFAULT_PROJECTION_LIMIT)
        .min(escurel_index::backend::MAX_PROJECTION_ROWS);
    let mut rows = indexer.project_view(view, limit + 1).await.ok()?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);

    // Expose projected source columns under `source.<overlay_field>` per the
    // skill's `project` map (drift-visible; overlay wins for display).
    let mut source = serde_json::Map::new();
    if let Some(sv) = binding.and_then(|b| b.sql_view)
        && let Some(first) = rows.first()
    {
        for (src_col, overlay_field) in &sv.project {
            if let Some(v) = first.get(src_col) {
                source.insert(overlay_field.clone(), v.clone());
            }
        }
    }

    Some(json!({
        "view": view,
        "rows": rows,
        "source": source,
        "truncated": truncated,
    }))
}

#[derive(Deserialize)]
pub(super) struct NeighboursArgs {
    page_id: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    link_skill: Option<String>,
    /// RFC 3339 time-travel cut; edges from sources born after it are hidden.
    #[serde(default)]
    as_of: Option<String>,
    /// Scenario overlay; edges are filtered by their source page's scenario.
    #[serde(default)]
    scenario: Option<String>,
}

pub(super) async fn tool_neighbours(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: NeighboursArgs = parse_args(args, "neighbours")?;
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
        .neighbours(
            &a.page_id,
            dir,
            a.link_skill.as_deref(),
            a.as_of.as_deref(),
            a.scenario.as_deref(),
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("neighbours: {e}")))?;
    // ACL (always on): drop edges whose OTHER endpoint is an owner-private
    // instance the caller can't read — don't reveal links to/from private
    // records. The queried page itself is the caller's vantage point.
    let mut out = Vec::with_capacity(edges.len());
    for e in &edges {
        let neighbour = if e.src_page == a.page_id {
            &e.dst_page
        } else {
            &e.src_page
        };
        let readable = match indexer
            .expand(neighbour, None, None)
            .await
            .map_err(|err| JsonRpcError::internal(format!("neighbours acl: {err}")))?
        {
            Some(ex) if ex.page.page_type == PageType::Instance => indexer
                .may_read_instance(&caller, &ex.page.skill, &ex.frontmatter)
                .await
                .map_err(|err| JsonRpcError::internal(format!("neighbours acl: {err}")))?,
            _ => true, // non-instance / absent → not owner-gated
        };
        if readable {
            out.push(json!({
                "src_page": e.src_page,
                "dst_page": e.dst_page,
                "link_skill": e.link_skill,
                "link_version": e.link_version,
                "dst_anchor": e.dst_anchor,
            }));
        }
    }
    Ok(json!({ "edges": out }))
}

/// Default hop depth when the caller omits `max_hops`.
pub(super) const PROVENANCE_DEFAULT_HOPS: u32 = 5;

#[derive(Deserialize)]
pub(super) struct ProvenanceAncestryArgs {
    #[serde(alias = "from_page", alias = "from_page_id")]
    page_id: String,
    /// Target page: switches to reachability/shortest-path mode — the
    /// old `provenance_path` contract, folded in here (API review).
    #[serde(default, alias = "to_page_id")]
    to_page: Option<String>,
    /// `up` (everything this rests on) | `down` (everything derived from it).
    #[serde(default)]
    direction: Option<String>,
    /// Restrict the walk to these edge kinds; empty/absent = all.
    #[serde(default)]
    relations: Option<Vec<String>>,
    /// Hop ceiling; defaults to `PROVENANCE_DEFAULT_HOPS`, clamped server-side.
    #[serde(default)]
    max_hops: Option<u32>,
    /// RFC 3339 time-travel cut; edges from sources born after it are hidden.
    #[serde(default)]
    as_of: Option<String>,
}

pub(super) async fn tool_provenance_ancestry(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ProvenanceAncestryArgs = parse_args(args, "provenance_ancestry")?;
    let dir = match a.direction.as_deref().unwrap_or("up") {
        "up" => GraphDir::Up,
        "down" => GraphDir::Down,
        other => {
            return Err(JsonRpcError::invalid_params(format!(
                "provenance_ancestry direction `{other}`; expected up|down"
            )));
        }
    };
    let relations = a.relations.unwrap_or_default();
    let rel_opt = (!relations.is_empty()).then_some(relations.as_slice());

    // `to_page` switches to the PATH question (the old `provenance_path`
    // tool, folded in): does `page_id` reach `to_page` within `max_hops`?
    // Fail-closed: a path is disclosed only if EVERY node on it is
    // readable — a single private node reports `reachable: false` with no
    // path, never confirming a connection through a hidden record.
    if let Some(to_page) = a.to_page.as_deref().filter(|t| !t.is_empty()) {
        let found = indexer
            .provenance_path(
                &a.page_id,
                to_page,
                dir,
                rel_opt,
                a.max_hops.unwrap_or(PROVENANCE_DEFAULT_HOPS),
            )
            .await
            .map_err(|e| JsonRpcError::internal(format!("provenance_ancestry path: {e}")))?;
        let none = json!({ "reachable": false, "path": [], "depth": 0 });
        let Some(p) = found else {
            return Ok(none);
        };
        for pid in &p.path {
            if !provenance_page_readable(indexer, &caller, pid).await? {
                return Ok(none);
            }
        }
        return Ok(json!({ "reachable": true, "path": p.path, "depth": p.depth }));
    }

    let hops = indexer
        .provenance_ancestry(
            &a.page_id,
            dir,
            rel_opt,
            a.max_hops.unwrap_or(PROVENANCE_DEFAULT_HOPS),
            a.as_of.as_deref(),
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("provenance_ancestry: {e}")))?;

    // ACL (always on): resolve the readability of every page touched — the
    // reached nodes AND the interior nodes of each path — then drop any hop
    // whose path crosses an owner-private instance the caller can't read
    // (fail-closed transitive visibility). The start page (path[0]) is the
    // caller's own vantage point and is not gated, mirroring `neighbours`.
    let mut readable: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    for h in &hops {
        for pid in h.path.iter().skip(1) {
            if !readable.contains_key(pid) {
                let r =
                    match indexer.expand(pid, None, None).await.map_err(|e| {
                        JsonRpcError::internal(format!("provenance_ancestry acl: {e}"))
                    })? {
                        Some(ex) if ex.page.page_type == PageType::Instance => indexer
                            .may_read_instance(&caller, &ex.page.skill, &ex.frontmatter)
                            .await
                            .map_err(|e| {
                                JsonRpcError::internal(format!("provenance_ancestry acl: {e}"))
                            })?,
                        _ => true,
                    };
                readable.insert(pid.clone(), r);
            }
        }
    }
    // Emit one row per node at its shallowest depth (rows arrive ordered by
    // depth), keeping only hops whose whole path (past the vantage) is
    // readable.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for h in &hops {
        if !h
            .path
            .iter()
            .skip(1)
            .all(|p| *readable.get(p).unwrap_or(&false))
        {
            continue;
        }
        if seen.insert(h.page_id.clone()) {
            out.push(json!({
                "page_id": h.page_id,
                "skill": h.skill,
                "relation": h.relation,
                "depth": h.depth,
            }));
        }
    }
    Ok(json!({ "hops": out }))
}

/// Whether `page_id` is readable by `caller`: a non-instance / absent page
/// is not owner-gated (true); an instance goes through `may_read_instance`
/// (fail-closed). Shared by the provenance analytics ACL filters.
pub(super) async fn provenance_page_readable(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    page_id: &str,
) -> Result<bool, JsonRpcError> {
    match indexer
        .expand(page_id, None, None)
        .await
        .map_err(|e| JsonRpcError::internal(format!("provenance acl: {e}")))?
    {
        Some(ex) if ex.page.page_type == PageType::Instance => indexer
            .may_read_instance(caller, &ex.page.skill, &ex.frontmatter)
            .await
            .map_err(|e| JsonRpcError::internal(format!("provenance acl: {e}"))),
        _ => Ok(true),
    }
}

#[derive(Deserialize)]
pub(super) struct ProvenanceReportArgs {
    /// `drift` (decisions resting on a superseded expectation) or
    /// `abandoned` (nodes retired via supersedes/abandons).
    kind: String,
    /// Restrict to this skill; absent/empty = all.
    #[serde(default)]
    skill: Option<String>,
}

/// `provenance_report`: the corpus-wide ADR-0010 analytics, consolidated
/// from the old `expectation_drift` / `abandoned_paths` tools (API
/// review, minimalism finding 4). One normalized `{kind, rows}` shape;
/// rows touching an ACL-private page are dropped, fail-closed.
pub(super) async fn tool_provenance_report(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ProvenanceReportArgs = parse_args(args, "provenance_report")?;
    let skill = a.skill.filter(|s| !s.is_empty());
    match a.kind.as_str() {
        "drift" => {
            let rows = indexer
                .expectation_drift(skill.as_deref())
                .await
                .map_err(|e| JsonRpcError::internal(format!("provenance_report: {e}")))?;
            let mut out = Vec::new();
            for r in &rows {
                let mut visible = true;
                for pid in [
                    &r.decision_page_id,
                    &r.expectation_page_id,
                    &r.superseding_page_id,
                ] {
                    if !provenance_page_readable(indexer, &caller, pid).await? {
                        visible = false;
                        break;
                    }
                }
                if visible {
                    out.push(json!({
                        "decision_page_id": r.decision_page_id,
                        "decision_skill": r.decision_skill,
                        "expectation_page_id": r.expectation_page_id,
                        "superseding_page_id": r.superseding_page_id,
                        "decided_at": r.decided_at,
                        "superseded_at": r.superseded_at,
                    }));
                }
            }
            Ok(json!({ "kind": "drift", "rows": out }))
        }
        "abandoned" => {
            let nodes = indexer
                .abandoned_paths(skill.as_deref())
                .await
                .map_err(|e| JsonRpcError::internal(format!("provenance_report: {e}")))?;
            let mut out = Vec::new();
            for n in &nodes {
                if provenance_page_readable(indexer, &caller, &n.page_id).await? {
                    out.push(json!({ "page_id": n.page_id, "skill": n.skill, "via": n.via }));
                }
            }
            Ok(json!({ "kind": "abandoned", "rows": out }))
        }
        other => Err(JsonRpcError::invalid_params(format!(
            "provenance_report kind `{other}`; expected drift|abandoned"
        ))),
    }
}

#[derive(Deserialize)]
pub(super) struct SearchArgs {
    /// Single query string (unchanged). Optional now that `queries`
    /// exists; at least one of `q` / `queries` must be present.
    #[serde(default)]
    pub(super) q: Option<String>,
    /// Multi-query variants (#217 Part 2). When supplied, each variant
    /// is embedded and run through both lanes; their ACL-filtered
    /// candidate lists are RRF-fused into one ranking before rerank.
    #[serde(default)]
    pub(super) queries: Option<Vec<String>>,
    #[serde(default = "default_k")]
    pub(super) k: usize,
    #[serde(default)]
    pub(super) page_type: Option<String>,
    #[serde(default, alias = "skill_id")]
    pub(super) skill: Option<String>,
    /// RFC 3339 time-travel cut; blocks born after it are excluded.
    #[serde(default)]
    pub(super) as_of: Option<String>,
    /// Scenario overlay; base-only when null/absent.
    #[serde(default)]
    pub(super) scenario: Option<String>,
    /// `"block"` (default) or `"page"`.
    #[serde(default)]
    pub(super) granularity: Option<String>,
    /// Frontmatter post-filter object (see `escurel_index::filter`).
    #[serde(default)]
    pub(super) filter: Option<Value>,
    /// Restrict the search to a single page's blocks (relevance heatmap).
    #[serde(default)]
    pub(super) page_id: Option<String>,
}

pub(super) fn default_k() -> usize {
    10
}

/// Upper bound on query variants fused in one `search` call — guards
/// against an unbounded fan-out of first-stage retrievals.
pub(super) const MAX_QUERY_VARIANTS: usize = 8;

/// The de-duplicated, order-preserving list of query variants to run.
/// Falls back to the scalar `q` when `queries` is absent/empty; errors
/// when neither yields a non-empty string. Capped at
/// [`MAX_QUERY_VARIANTS`].
pub(super) fn effective_queries(a: &SearchArgs) -> Result<Vec<String>, JsonRpcError> {
    let mut variants: Vec<String> = a
        .queries
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if variants.is_empty()
        && let Some(q) = a.q.as_deref().map(str::trim).filter(|s| !s.is_empty())
    {
        variants.push(q.to_owned());
    }
    if variants.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "search: provide `q` or a non-empty `queries`",
        ));
    }
    // Drop duplicate phrasings so the same query can't double-weight a
    // page through RRF; preserve first-seen order.
    let mut seen = std::collections::HashSet::new();
    variants.retain(|v| seen.insert(v.clone()));
    variants.truncate(MAX_QUERY_VARIANTS);
    Ok(variants)
}

pub(crate) async fn tool_search(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: SearchArgs = parse_args(args, "search")?;
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
    let granularity = Granularity::from_arg(a.granularity.as_deref().unwrap_or_default());
    // An empty `{}` filter is treated as "no filter".
    let filter = a.filter.as_ref().filter(|f| !is_empty_filter(f));

    // Query variants (#217 Part 2): one for a scalar `q`, or several for
    // a `queries` list. Their candidate lists are fused below.
    let variants = effective_queries(&a)?;

    // When the rerank stage is on, fetch a larger fused candidate pool
    // so the cross-encoder has more than the caller's `k` to reorder;
    // we truncate back to `k` after reranking. With rerank off this is
    // `a.k`, so the single-variant path stays byte-identical to before.
    let pool = indexer.rerank_candidate_pool(a.k);

    // The SQL-view lane is skipped when the search is restricted to skills,
    // OR when the caller set constraints the late-materialised lane does not
    // yet honor (`as_of` time-travel, `scenario` overlay, frontmatter
    // `filter`, `page_id`) — fusing unconstrained SQL hits would violate
    // them, so skip the lane (conservative + correct) until it can apply
    // the constraints.
    let constrained =
        a.as_of.is_some() || a.scenario.is_some() || filter.is_some() || a.page_id.is_some();
    let sql_lane_enabled = !matches!(pt, Some(PageType::Skill)) && !constrained;
    let page_id = a.page_id.as_deref().filter(|s| !s.is_empty());

    // Run every variant through both lanes. INV-ACL-FUSION (spike S3):
    // EVERY lane's contribution — for EVERY variant — is ACL-filtered
    // BEFORE it enters the fusion. Deterministic ACL drops owner-private
    // hits the caller does not own (admin bypasses); skill pages are the
    // public catalogue.
    let mut lanes: Vec<Vec<escurel_index::SearchHit>> = Vec::with_capacity(variants.len() * 2);
    for q in &variants {
        let native = indexer
            .search_with(
                q,
                pool,
                pt,
                a.skill.as_deref(),
                a.as_of.as_deref(),
                a.scenario.as_deref(),
                granularity,
                filter,
                page_id,
            )
            .await
            .map_err(|e| JsonRpcError::internal(format!("search: {e}")))?;
        lanes.push(acl_filter_hits(indexer, &caller, native).await?);

        if sql_lane_enabled {
            let candidates = indexer
                .sql_view_search_candidates(q, a.skill.as_deref())
                .await
                .map_err(|e| JsonRpcError::internal(format!("search sql lane: {e}")))?;
            let sql_allowed = acl_filter_hits(indexer, &caller, candidates).await?;
            if !sql_allowed.is_empty() {
                lanes.push(sql_allowed);
            }
        }
    }

    // A single lane (one variant, no SQL contribution) is returned verbatim
    // — the markdown-only single-query behaviour is byte-identical to before.
    // Otherwise RRF-fuse all variant × lane lists into one ranking (over the
    // wider `pool` so the rerank stage sees them all).
    let fused = if lanes.len() == 1 {
        lanes.pop().unwrap_or_default()
    } else {
        rrf_fuse_many(lanes, pool)
    };

    // Cross-encoder rerank — runs AFTER the per-lane ACL filter and RRF
    // fusion (INV-ACL-FUSION): it only reorders rows the caller may
    // already see. A no-op when rerank is disabled. Reranks against the
    // primary variant, then truncate to the caller's `k`.
    let mut final_hits = indexer
        .rerank_hits(&variants[0], fused)
        .await
        .map_err(|e| JsonRpcError::internal(format!("search rerank: {e}")))?;
    final_hits.truncate(a.k);

    let out: Vec<Value> = final_hits
        .iter()
        .map(|h| {
            json!({
                "page_id": h.page_id,
                "slug": h.slug,
                "skill": h.skill,
                "page_type": page_type_str(h.page_type),
                "anchor": h.anchor,
                "snippet": h.snippet,
                "score": h.score,
                "similarity": h.similarity,
                "frontmatter_excerpt": h.frontmatter_excerpt,
            })
        })
        .collect();
    Ok(json!({
        "hits": out,
        "granularity": granularity.as_str(),
    }))
}

/// Apply the fail-closed per-instance read ACL to one lane's candidates,
/// preserving order. Skill-page hits are the public catalogue (never gated).
/// A SQL-view instance whose `owner_field` cannot be resolved fails closed
/// inside `may_read_instance` (deny to non-admins).
pub(super) async fn acl_filter_hits(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    hits: Vec<escurel_index::SearchHit>,
) -> Result<Vec<escurel_index::SearchHit>, JsonRpcError> {
    let mut out = Vec::with_capacity(hits.len());
    for h in hits {
        if h.page_type == PageType::Instance
            && !indexer
                .may_read_instance(caller, &h.skill, &h.frontmatter_excerpt)
                .await
                .map_err(|e| JsonRpcError::internal(format!("search acl: {e}")))?
        {
            continue;
        }
        out.push(h);
    }
    Ok(out)
}

/// Reciprocal-Rank-Fusion of N already-ACL-filtered, already-ranked lanes
/// into a page-grain top-`cap`. Each lane contributes `1/(K_RRF + rank)`
/// per page; the first lane to surface a page is its representative on a
/// collision (lanes are pushed native-before-SQL, primary-variant-first,
/// so the strongest source wins the representative). Generalises the
/// two-lane fuse to the multi-query case (#217 Part 2).
pub(super) fn rrf_fuse_many(
    lanes: Vec<Vec<escurel_index::SearchHit>>,
    cap: usize,
) -> Vec<escurel_index::SearchHit> {
    use std::collections::HashMap;
    const K_RRF: f64 = 60.0;
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut rep: HashMap<String, escurel_index::SearchHit> = HashMap::new();
    for lane in lanes {
        for (rank, h) in lane.into_iter().enumerate() {
            *scores.entry(h.page_id.clone()).or_insert(0.0) += 1.0 / (K_RRF + (rank as f64) + 1.0);
            rep.entry(h.page_id.clone()).or_insert(h);
        }
    }
    let mut fused: Vec<escurel_index::SearchHit> = rep
        .into_values()
        .map(|mut h| {
            h.score = scores[&h.page_id];
            h
        })
        .collect();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.page_id.cmp(&b.page_id))
    });
    fused.truncate(cap);
    fused
}

/// True for `null` or an empty `{}` filter object — both mean "no
/// post-filter", so we skip the work and the `Some`/`None` plumbing.
pub(super) fn is_empty_filter(f: &Value) -> bool {
    f.is_null() || f.as_object().is_some_and(serde_json::Map::is_empty)
}

#[derive(Deserialize)]
pub(super) struct QueryInstanceArgs {
    /// The query page: a bare id, `query::id`, or its `[[query::id]]`
    /// wikilink. `ref` is the documented key; `query_id` is accepted as an
    /// alias (the retired `run_stored_query`'s spelling — kept so its
    /// migrating callers bind).
    #[serde(rename = "ref", alias = "query_id")]
    query_ref: String,
    #[serde(default)]
    params: serde_json::Map<String, Value>,
}

/// Normalise a query reference to the bare slug the indexer expects:
/// `[[query::sales]]` / `query::sales` / `sales` all become `sales`.
pub(super) fn normalize_query_ref(raw: &str) -> String {
    let s = raw.trim();
    let s = s.strip_prefix("[[").unwrap_or(s);
    let s = s.strip_suffix("]]").unwrap_or(s);
    s.strip_prefix("query::").unwrap_or(s).to_owned()
}

pub(super) async fn tool_query_instance(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: QueryInstanceArgs = parse_args(args, "query_instance")?;
    let query_id = normalize_query_ref(&a.query_ref);
    let out = indexer
        .query_instance(&query_id, &a.params, &caller)
        .await
        .map_err(|e| JsonRpcError::internal(format!("query_instance: {e}")))?;
    Ok(json!({
        "rows": out.rows,
        "schema": out.schema.iter().map(|c| json!({
            "name": c.name,
            "type": c.type_name,
        })).collect::<Vec<_>>(),
        "truncated": out.truncated,
    }))
}

#[derive(Deserialize)]
pub(super) struct ValidateArgs {
    content: String,
    #[serde(default)]
    as_page_id: Option<String>,
}

pub(super) async fn tool_validate(indexer: &Indexer, args: Value) -> Result<Value, JsonRpcError> {
    let a: ValidateArgs = parse_args(args, "validate")?;
    let issues = indexer
        .validate(a.as_page_id.as_deref(), &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("validate: {e}")))?;
    // `ok` is false iff any issue is error-severity, mirroring the
    // documented ValidateResponse contract (warnings/infos don't fail
    // a draft). The wire carries both `ok` and the full issue list.
    let ok = !issues.iter().any(|i| i.severity == Severity::Error);
    Ok(json!({
        "ok": ok,
        "issues": issues.iter().map(issue_to_json).collect::<Vec<_>>(),
    }))
}

/// Map a domain [`Issue`] to the MCP JSON shape from
/// `docs/spec/protocol.md §Issue`.
pub(super) fn issue_to_json(issue: &Issue) -> Value {
    let mut obj = json!({
        "severity": issue.severity.as_str(),
        "code": issue.code,
        "location": issue.location,
        "message": issue.message,
    });
    if let Some(s) = &issue.suggestion {
        obj["suggestion"] = json!(s);
    }
    obj
}
