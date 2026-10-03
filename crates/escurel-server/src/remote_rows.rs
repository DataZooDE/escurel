//! `instances: rows` over a REMOTE upstream (stage 4): every object of a REST service, or of an
//! MCP tool's listing, is an instance, with an optional linked markdown page.
//!
//! Like the SQL rows (`escurel-index::backend::rows`) the rows are VIRTUAL: no page is stored per
//! object. A row's id is the encoded key; its page id is `markdown/instances/<skill>/<id>.md`, the
//! same id its linked page would have. What this module adds is the network: listing pages by the
//! upstream's own cursor, reading one object, and the write guard that keeps a row's source fields
//! out of its linked markdown.
//!
//! Everything read from the upstream is EXTERNAL data: it comes back marked `trust: "external"`.

use escurel_index::Indexer;
use escurel_index::backend::remote::{json_path_get, resolve_projection};
use escurel_index::backend::rows::RowsWriteRejection;
use escurel_index::backend::rows::{ROWS_MAX_LIMIT, decode_row_id, encode_row_id};
use escurel_index::backend::{RemoteBinding, RemoteList, RowsConfig};
use escurel_index::endpoints::EndpointRecord;
use serde_json::{Map, Value};

use crate::egress::Egress;
use crate::remote_backend;

/// Default and maximum page size for a remote list (REST/MCP pages are small by design).
pub(crate) const REMOTE_DEFAULT_LIMIT: usize = 50;
pub(crate) const REMOTE_MAX_LIMIT: usize = 200;

/// A remote `rows` skill's resolved binding.
pub(crate) struct RemoteRows {
    pub skill: String,
    pub remote: RemoteBinding,
    pub cfg: RowsConfig,
    pub list: RemoteList,
    pub ep: EndpointRecord,
    /// JSON path of the identity in a listed object (`backend.key`, e.g. `$.id`).
    pub key_path: String,
    pub limiter_key: String,
}

/// One object, as an instance.
pub(crate) struct RemoteRow {
    pub id: String,
    pub page_id: String,
    /// The projected fields under their frontmatter names.
    pub fields: Map<String, Value>,
}

/// The skill's remote rows binding, or `None` when it is not a remote `rows` skill. A remote rows
/// skill that cannot be used (no `list`, no `key`, endpoint not registered) is an error that names
/// what is missing; it never falls back to per-instance behaviour.
pub(crate) async fn source(indexer: &Indexer, skill: &str) -> Result<Option<RemoteRows>, String> {
    let b = indexer
        .skill_backend(skill)
        .await
        .map_err(|e| e.to_string())?;
    let (Some(cfg), Some(remote)) = (b.rows, b.remote) else {
        return Ok(None);
    };
    let list = remote
        .list
        .clone()
        .ok_or_else(|| format!("skill `{skill}` declares `instances: rows` but no `list:` op"))?;
    let key_path = cfg
        .key
        .first()
        .cloned()
        .ok_or_else(|| format!("skill `{skill}` declares `instances: rows` but no `key:`"))?;
    let ep = indexer
        .lookup_endpoint(&remote.endpoint)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("endpoint `{}` is not registered", remote.endpoint))?;
    let limiter_key = remote_backend::endpoint_key(indexer, &ep.name);
    Ok(Some(RemoteRows {
        skill: skill.to_owned(),
        remote,
        cfg,
        list,
        ep,
        key_path,
        limiter_key,
    }))
}

fn row_from(src: &RemoteRows, item: &Value) -> Option<RemoteRow> {
    let key = match json_path_get(item, &src.key_path)? {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let id = encode_row_id(&[key]);
    let fields = resolve_projection(item, &src.remote.project);
    Some(RemoteRow {
        page_id: format!("markdown/instances/{}/{id}.md", src.skill),
        id,
        fields,
    })
}

/// The client's cursor token is the upstream's cursor, hex-encoded: opaque to the client, and
/// anything that is not valid hex is refused before it goes anywhere.
fn encode_cursor(upstream: &str) -> String {
    upstream.bytes().map(|b| format!("{b:02x}")).collect()
}

fn decode_cursor(token: &str) -> Result<String, String> {
    if !token.len().is_multiple_of(2) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid cursor".to_owned());
    }
    let bytes: Result<Vec<u8>, _> = (0..token.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&token[i..i + 2], 16))
        .collect();
    String::from_utf8(bytes.map_err(|_| "invalid cursor".to_owned())?)
        .map_err(|_| "invalid cursor".to_owned())
}

/// One page of the upstream's objects. `Err` carries a message that is safe to show (no URL, no
/// upstream body); `invalid cursor` is the only caller mistake.
pub(crate) async fn list(
    egress: &Egress,
    src: &RemoteRows,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> Result<(Vec<RemoteRow>, Option<String>), String> {
    let upstream_cursor = cursor.map(decode_cursor).transpose()?;
    let limit = limit
        .unwrap_or(REMOTE_DEFAULT_LIMIT)
        .clamp(1, REMOTE_MAX_LIMIT.min(ROWS_MAX_LIMIT));
    let resp = remote_backend::call_list(
        egress,
        &src.limiter_key,
        &src.ep,
        &src.remote,
        &src.list,
        upstream_cursor.as_deref(),
        limit,
    )
    .await?;
    let items = json_path_get(&resp, &src.list.items)
        .and_then(Value::as_array)
        .ok_or_else(|| "the upstream list response has no items array".to_owned())?;
    let rows: Vec<RemoteRow> = items
        .iter()
        .take(limit)
        .filter_map(|item| row_from(src, item))
        .collect();
    let next = src
        .list
        .cursor
        .as_ref()
        .and_then(|c| json_path_get(&resp, &c.from))
        .and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .map(|c| encode_cursor(&c));
    Ok((rows, next))
}

/// One object by instance id. `None` when the upstream says it has no such object (404) or the id
/// is not a well-formed encoding.
pub(crate) async fn get(
    egress: &Egress,
    src: &RemoteRows,
    id: &str,
) -> Result<Option<RemoteRow>, String> {
    let Some(parts) = decode_row_id(id, 1) else {
        return Ok(None);
    };
    match remote_backend::call_read(egress, &src.limiter_key, &src.ep, &src.remote, &parts[0]).await
    {
        // An MCP read tool answers "no such object" with an empty result, not a 404.
        Ok(item)
            if src.remote.kind == escurel_index::RemoteKind::Mcp
                && resolve_projection(&item, &src.remote.project).is_empty() =>
        {
            Ok(None)
        }
        Ok(item) => Ok(row_from(src, &item).or_else(|| {
            // The read response may not repeat the key; the id we asked for is the identity.
            let fields = resolve_projection(&item, &src.remote.project);
            Some(RemoteRow {
                id: id.to_owned(),
                page_id: format!("markdown/instances/{}/{id}.md", src.skill),
                fields,
            })
        })),
        Err(e) if e == "upstream status 404" => Ok(None),
        Err(e) => Err(e),
    }
}

/// Like [`get`], plus the upstream's own `ETag` for REST (the `If-Match` of a later write).
pub(crate) async fn get_with_etag(
    egress: &Egress,
    src: &RemoteRows,
    id: &str,
) -> Result<Option<(RemoteRow, Option<String>)>, String> {
    let Some(parts) = decode_row_id(id, 1) else {
        return Ok(None);
    };
    match remote_backend::call_read_etag(egress, &src.limiter_key, &src.ep, &src.remote, &parts[0])
        .await
    {
        Ok((item, etag)) => {
            let fields = resolve_projection(&item, &src.remote.project);
            if src.remote.kind == escurel_index::RemoteKind::Mcp && fields.is_empty() {
                return Ok(None);
            }
            Ok(Some((
                RemoteRow {
                    id: id.to_owned(),
                    page_id: format!("markdown/instances/{}/{id}.md", src.skill),
                    fields,
                },
                etag,
            )))
        }
        Err(e) if e == "upstream status 404" => Ok(None),
        Err(e) => Err(e),
    }
}

/// The write guard for a page id inside a remote `rows` skill (the counterpart of the SQL rows'
/// `rows_write_rejection`): `None` when the write may go ahead. A row page is the object's LINKED
/// MARKDOWN, so a write needs `linked`, an existing object (or an existing companion), no
/// server-managed `backend_ref`, and no projected SOURCE field in its frontmatter.
pub(crate) async fn write_rejection(
    indexer: &Indexer,
    egress: &Egress,
    page_id: &str,
    content: &str,
    allow_intent: bool,
) -> Result<Option<RowsWriteRejection>, String> {
    let Some((skill, id)) = escurel_index::backend::rows::split_instance_page_id(page_id) else {
        return Ok(None);
    };
    let Some(src) = source(indexer, skill).await? else {
        return Ok(None);
    };
    // A `write_back` intent is a human-gated instruction to the upstream: it may only travel in a
    // DRAFT (the promote hook is the gate), and only for the columns the skill declares writable.
    if let Ok(parsed) = escurel_md::parse(content)
        && parsed.frontmatter.fields.contains_key("write_back")
    {
        if !allow_intent {
            return Ok(Some(RowsWriteRejection {
                code: "write_back_requires_draft",
                location: "frontmatter.write_back".to_owned(),
                message: "a `write_back` change must be proposed as a draft: a human promotes it, \
                          and only then does it reach the upstream"
                    .to_owned(),
            }));
        }
        if src.remote.write.is_none() {
            return Ok(Some(RowsWriteRejection {
                code: "backend_read_only",
                location: "frontmatter.write_back".to_owned(),
                message: format!(
                    "skill `{skill}` declares no `write` op; its rows cannot be written back"
                ),
            }));
        }
        match crate::write_back::parse_intent(&crate::write_back::frontmatter_json(
            &parsed.frontmatter.fields,
        )) {
            Err(m) => {
                return Ok(Some(RowsWriteRejection {
                    code: "write_back_invalid",
                    location: "frontmatter.write_back".to_owned(),
                    message: m,
                }));
            }
            Ok(Some(intent)) => {
                for f in intent.patch.keys() {
                    if !src.cfg.writable_columns.contains(f) {
                        return Ok(Some(RowsWriteRejection {
                            code: "backend_read_only_field",
                            location: format!("frontmatter.write_back.patch.{f}"),
                            message: format!(
                                "`{f}` is not a writable column of `{skill}` (writable: {:?})",
                                src.cfg.writable_columns
                            ),
                        }));
                    }
                }
            }
            Ok(None) => {}
        }
    }
    if !src.cfg.linked {
        return Ok(Some(RowsWriteRejection {
            code: "backend_read_only",
            location: "page_id".to_owned(),
            message: format!(
                "skill `{skill}` is a read-only `rows` backend without `linked` markdown; its \
                 rows cannot be written"
            ),
        }));
    }
    let companion_exists = indexer
        .read_page_markdown(page_id)
        .await
        .map_err(|e| e.to_string())?
        .is_some();
    if !companion_exists && get(egress, &src, id).await?.is_none() {
        return Ok(Some(RowsWriteRejection {
            code: "row_not_found",
            location: "page_id".to_owned(),
            message: format!("`{skill}` has no object with the key `{id}` upstream"),
        }));
    }
    let Ok(parsed) = escurel_md::parse(content) else {
        return Ok(None);
    };
    let fields = &parsed.frontmatter.fields;
    if fields.contains_key("backend_ref") {
        return Ok(Some(RowsWriteRejection {
            code: "backend_read_only",
            location: "frontmatter.backend_ref".to_owned(),
            message: "`backend_ref` is server-managed; a row's companion page cannot carry one"
                .to_owned(),
        }));
    }
    for f in src.remote.project.keys() {
        if fields.contains_key(f.as_str()) {
            return Ok(Some(RowsWriteRejection {
                code: "backend_read_only_field",
                location: format!("frontmatter.{f}"),
                message: format!(
                    "`{f}` is a source field of `{skill}` and read-only; keep your own fields \
                     and the body in the companion page instead"
                ),
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_round_trips_and_garbage_is_refused() {
        for raw in ["c-0199", "a&b=1#x", "ünï", "eyJhIjoxfQ=="] {
            assert_eq!(decode_cursor(&encode_cursor(raw)).unwrap(), raw);
        }
        for bad in ["zz", "abc", "x&y", "1g"] {
            assert!(decode_cursor(bad).is_err(), "{bad}");
        }
    }
}
