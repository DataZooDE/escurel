//! Parsing the per-skill `backend:` frontmatter block.
//!
//! A skill page MAY declare a `backend:` block selecting which
//! [`InstanceBackend`](super::InstanceBackend) materialises and reads its
//! instances. A skill with no `backend:` block — every skill in the corpus
//! today — defaults to [`BackendKind::Markdown`], so this is fully
//! backward-compatible (REQ-BK-01).
//!
//! PR-1 recognised only `kind: markdown`; PR-2 adds the `sql_view` arm and
//! its `source` / `project` / `search_text` configuration (REQ-SQL-01). The
//! `document` arm lands with its backend. Unknown kinds fall back to
//! markdown here (read-path lenience); the create/validate path rejects a
//! malformed binding with a typed `Issue` instead.

use std::collections::BTreeMap;

use super::BackendKind;

/// The parsed `backend:` block off a skill page's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackendBinding {
    pub kind: BackendKind,
    /// Present (and `kind == SqlView`) when the skill declares a
    /// `backend.source` SQL-view binding (REQ-SQL-01).
    pub sql_view: Option<SqlViewBinding>,
    /// Present (and `kind == Document`) when the skill declares a
    /// `backend.accepts` document binding (REQ-DOC-01).
    pub document: Option<DocumentBinding>,
    /// Present (and `kind ∈ {OpenApi, Mcp}`) when the skill declares a live
    /// remote (proxy) backend — an `endpoint` + `read`/`write` op + `project`
    /// map (REQ-REMOTE-01). `None` for a remote kind whose block is missing
    /// required fields, so the create/validate path fails closed rather than
    /// the read path panicking (mirrors `sql_view`).
    pub remote: Option<RemoteBinding>,
    /// `sql_view` read cap: the maximum rows `expand` renders in the bounded
    /// projection (`backend.projection_limit`). `None` ⇒ the server default.
    pub projection_limit: Option<usize>,
}

/// Which remote-proxy protocol a `RemoteBinding` speaks. Mirrors the
/// `BackendKind` remote arms, kept local so [`RemoteOp`] parsing is
/// self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    /// REST/HTTP endpoint described by an OpenAPI document.
    OpenApi,
    /// Upstream MCP server (escurel is the client).
    Mcp,
}

impl RemoteKind {
    /// The registry `kind` string this maps to (`external_endpoints.kind`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenApi => "openapi",
            Self::Mcp => "mcp",
        }
    }
}

/// A single remote operation — how a `read` or `write` reaches the upstream.
/// The variant is selected by the binding's [`RemoteKind`]: `openapi` uses
/// [`RemoteOp::Http`]; `mcp` uses [`RemoteOp::McpTool`] or
/// [`RemoteOp::McpResource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOp {
    /// OpenAPI/REST: an HTTP `method` + `path` template. The path MAY contain a
    /// `{id}` segment, filled from the overlay instance id; other placeholders
    /// are not yet bound (see `docs/spec/protocol.md` §"Remote backends").
    Http { method: String, path: String },
    /// MCP: call a tool by `name` (arguments are the write payload / id map).
    McpTool { name: String },
    /// MCP: read a resource by `uri` template.
    McpResource { uri: String },
}

/// A live remote (proxy) backend's binding (REQ-REMOTE-01): the
/// admin-registered `endpoint` name (the base URL + auth live server-side,
/// never in markdown — the SSRF / secrets-in-markdown guard), the `read` op
/// that fetches the projection, an optional `write` op for write-back, and a
/// `project` map from response JSON (dotted `$.a.b` path or bare top-level
/// key) to overlay frontmatter field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBinding {
    pub kind: RemoteKind,
    /// Admin-registered endpoint name (`backend.endpoint`) — resolved to a
    /// base URL + auth via the `external_endpoints` registry. NOT a raw URL.
    pub endpoint: String,
    /// How `expand` fetches the live projection.
    pub read: RemoteOp,
    /// How `write_instance` forwards a write upstream. `None` ⇒ read-only.
    pub write: Option<RemoteOp>,
    /// Response field → overlay frontmatter field. Value is a dotted JSON
    /// path (`$.a.b`) or a bare top-level key.
    pub project: BTreeMap<String, String>,
}

/// A `document` skill's intake config (REQ-DOC-01). `accepts` is the
/// MIME-dispatch key (REQ-DOC-06): an inbox arrival's content type selects
/// which document skill handles it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocumentBinding {
    /// MIME types this skill handles, e.g. `["application/pdf", "text/plain"]`.
    pub accepts: Vec<String>,
    /// Chunk sizing (`chunk.max_chars` / `chunk.overlap`); defaults applied
    /// by the worker when absent.
    pub max_chars: Option<usize>,
    pub overlap: Option<usize>,
    /// `duckdb` (default) | `lance` (per-skill escape hatch; v1 = duckdb).
    pub retrieval: Option<String>,
    /// Read cap: the maximum chunk lead `expand` returns (`backend.lead_chunks`).
    /// `None` ⇒ the server default. The full text always lives in the blob.
    pub lead_chunks: Option<usize>,
}

/// Which external source a `sql_view` skill materialises a read-only view
/// over (REQ-SQL-02).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlConnector {
    Postgres,
    Mysql,
    Sqlite,
    /// SAP via the ERPL community extension (unsigned — admin opt-in).
    Erpl,
    /// `read_json_auto` over a directory glob (core DuckDB, no extension).
    JsonDir,
    /// `read_parquet` over a directory glob (core DuckDB, no extension).
    ParquetDir,
}

impl SqlConnector {
    /// Parse the `connector:` wire value.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "postgres" => Self::Postgres,
            "mysql" => Self::Mysql,
            "sqlite" => Self::Sqlite,
            "erpl" => Self::Erpl,
            "json_dir" => Self::JsonDir,
            "parquet_dir" => Self::ParquetDir,
            _ => return None,
        })
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Sqlite => "sqlite",
            Self::Erpl => "erpl",
            Self::JsonDir => "json_dir",
            Self::ParquetDir => "parquet_dir",
        }
    }

    /// Directory-glob connectors read core DuckDB table functions and need
    /// no `ATTACH` and no registered credential. The database connectors
    /// (`postgres`/`mysql`/`sqlite`/`erpl`) `ATTACH` an external source and
    /// dereference an admin-registered credential (REQ-SQL-05).
    #[must_use]
    pub fn is_directory(self) -> bool {
        matches!(self, Self::JsonDir | Self::ParquetDir)
    }
}

/// A `sql_view` skill's `backend.source` + projection (REQ-SQL-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlViewBinding {
    pub connector: SqlConnector,
    /// Admin-registered credential name (`source.attach`) — NOT a DSN
    /// (REQ-SQL-05). `None` for directory connectors.
    pub attach: Option<String>,
    /// The source relation: `schema.table` for DB connectors, or a
    /// directory path/glob for `json_dir` / `parquet_dir`.
    pub relation: String,
    /// Optional `WHERE` clause spliced into the view (`source.filter`).
    pub filter: Option<String>,
    /// Source column → overlay frontmatter field (`project`).
    pub project: BTreeMap<String, String>,
    /// Columns whose text enters late-materialised FTS (`search_text`).
    pub search_text: Vec<String>,
}

impl BackendBinding {
    /// Parse the `backend:` block from a skill page's frontmatter JSON.
    ///
    /// Absent block / absent `kind:` ⇒ markdown. `kind: sql_view` parses
    /// the `source` / `project` / `search_text` sub-block into
    /// [`SqlViewBinding`]; a `sql_view` kind whose `source` is missing or
    /// has an unknown connector yields `kind = SqlView` with `sql_view =
    /// None`, so the create/validate path fails closed rather than the read
    /// path panicking. An unrecognised kind falls back to markdown.
    #[must_use]
    pub fn parse(fm: &serde_json::Value) -> Self {
        let Some(block) = fm.get("backend").and_then(serde_json::Value::as_object) else {
            return Self::default();
        };
        let read_usize = |key: &str| -> Option<usize> {
            block
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as usize)
        };
        match block.get("kind").and_then(serde_json::Value::as_str) {
            Some("markdown") | None => Self::default(),
            Some("sql_view") => Self {
                kind: BackendKind::SqlView,
                sql_view: parse_sql_view(block),
                document: None,
                remote: None,
                projection_limit: read_usize("projection_limit"),
            },
            Some("document") => Self {
                kind: BackendKind::Document,
                sql_view: None,
                document: Some(parse_document(block)),
                remote: None,
                projection_limit: None,
            },
            Some("openapi") => Self {
                kind: BackendKind::OpenApi,
                sql_view: None,
                document: None,
                remote: parse_remote(block, RemoteKind::OpenApi),
                projection_limit: read_usize("projection_limit"),
            },
            Some("mcp") => Self {
                kind: BackendKind::Mcp,
                sql_view: None,
                document: None,
                remote: parse_remote(block, RemoteKind::Mcp),
                projection_limit: read_usize("projection_limit"),
            },
            // Unknown kind: lenient on the read path (markdown); the
            // create/validate path is where a bad binding is rejected.
            Some(_) => Self::default(),
        }
    }
}

fn parse_document(block: &serde_json::Map<String, serde_json::Value>) -> DocumentBinding {
    let accepts = block
        .get("accepts")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let chunk = block.get("chunk").and_then(serde_json::Value::as_object);
    let usize_field = |key: &str| -> Option<usize> {
        chunk
            .and_then(|c| c.get(key))
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize)
    };
    DocumentBinding {
        accepts,
        max_chars: usize_field("max_chars").or_else(|| usize_field("max_tokens")),
        overlap: usize_field("overlap"),
        retrieval: block
            .get("retrieval")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        lead_chunks: block
            .get("lead_chunks")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize),
    }
}

/// Parse the `endpoint` / `read` / `write` / `project` block of a live
/// remote backend. Returns `None` (fail-closed at create) when `endpoint` is
/// absent or the required `read` op cannot be parsed for the given kind.
fn parse_remote(
    block: &serde_json::Map<String, serde_json::Value>,
    kind: RemoteKind,
) -> Option<RemoteBinding> {
    let endpoint = block
        .get("endpoint")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    let read = block
        .get("read")
        .and_then(serde_json::Value::as_object)
        .and_then(|op| parse_remote_op(op, kind, /* is_write */ false))?;
    let write = block
        .get("write")
        .and_then(serde_json::Value::as_object)
        .and_then(|op| parse_remote_op(op, kind, /* is_write */ true));
    let project = block
        .get("project")
        .and_then(serde_json::Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    Some(RemoteBinding {
        kind,
        endpoint,
        read,
        write,
        project,
    })
}

/// Parse one `read:` / `write:` op object into a [`RemoteOp`] for `kind`.
/// `openapi` needs a `path` (method defaults `GET` for read, `POST` for
/// write); `mcp` needs a `tool` or `resource`.
fn parse_remote_op(
    op: &serde_json::Map<String, serde_json::Value>,
    kind: RemoteKind,
    is_write: bool,
) -> Option<RemoteOp> {
    let get_str = |k: &str| op.get(k).and_then(serde_json::Value::as_str);
    match kind {
        RemoteKind::OpenApi => {
            let path = get_str("path")?.to_owned();
            let method = get_str("method")
                .map(str::to_ascii_uppercase)
                .unwrap_or_else(|| {
                    if is_write {
                        "POST".into()
                    } else {
                        "GET".into()
                    }
                });
            Some(RemoteOp::Http { method, path })
        }
        RemoteKind::Mcp => {
            if let Some(tool) = get_str("tool") {
                Some(RemoteOp::McpTool {
                    name: tool.to_owned(),
                })
            } else {
                get_str("resource").map(|uri| RemoteOp::McpResource {
                    uri: uri.to_owned(),
                })
            }
        }
    }
}

fn parse_sql_view(block: &serde_json::Map<String, serde_json::Value>) -> Option<SqlViewBinding> {
    let source = block.get("source").and_then(serde_json::Value::as_object)?;
    let connector = SqlConnector::from_wire(
        source
            .get("connector")
            .and_then(serde_json::Value::as_str)?,
    )?;
    let relation = source
        .get("relation")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    let attach = source
        .get("attach")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let filter = source
        .get("filter")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let project = block
        .get("project")
        .and_then(serde_json::Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let search_text = block
        .get("search_text")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(SqlViewBinding {
        connector,
        attach,
        relation,
        filter,
        project,
        search_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_backend_binding_absent_block_is_markdown() {
        assert_eq!(
            BackendBinding::parse(&json!({"type": "skill", "id": "customer"})),
            BackendBinding::default()
        );
    }

    #[test]
    fn parse_backend_binding_explicit_markdown() {
        assert_eq!(
            BackendBinding::parse(&json!({"backend": {"kind": "markdown"}})),
            BackendBinding::default()
        );
    }

    #[test]
    fn parse_backend_binding_unknown_kind_falls_back_to_markdown() {
        assert_eq!(
            BackendBinding::parse(&json!({"backend": {"kind": "wormhole"}})),
            BackendBinding::default()
        );
    }

    #[test]
    fn parse_sql_view_binding_full() {
        let fm = json!({
            "backend": {
                "kind": "sql_view",
                "source": {
                    "connector": "postgres",
                    "attach": "crm_pg",
                    "relation": "public.customers",
                    "filter": "region = 'EU'"
                },
                "project": { "name": "company_name", "tier": "account_tier" },
                "search_text": ["company_name", "notes"]
            }
        });
        let b = BackendBinding::parse(&fm);
        assert_eq!(b.kind, BackendKind::SqlView);
        let sv = b.sql_view.expect("sql_view binding present");
        assert_eq!(sv.connector, SqlConnector::Postgres);
        assert_eq!(sv.attach.as_deref(), Some("crm_pg"));
        assert_eq!(sv.relation, "public.customers");
        assert_eq!(sv.filter.as_deref(), Some("region = 'EU'"));
        assert_eq!(
            sv.project.get("name").map(String::as_str),
            Some("company_name")
        );
        assert_eq!(sv.search_text, vec!["company_name", "notes"]);
    }

    #[test]
    fn parse_sql_view_dir_connector_needs_no_attach() {
        let fm = json!({
            "backend": {
                "kind": "sql_view",
                "source": { "connector": "json_dir", "relation": "/data/customers" }
            }
        });
        let b = BackendBinding::parse(&fm);
        assert_eq!(b.kind, BackendKind::SqlView);
        let sv = b.sql_view.unwrap();
        assert_eq!(sv.connector, SqlConnector::JsonDir);
        assert!(sv.connector.is_directory());
        assert!(sv.attach.is_none());
    }

    #[test]
    fn parse_sql_view_missing_source_keeps_kind_but_no_binding() {
        // kind present but source absent → fail-closed at create, not here.
        let fm = json!({ "backend": { "kind": "sql_view" } });
        let b = BackendBinding::parse(&fm);
        assert_eq!(b.kind, BackendKind::SqlView);
        assert!(b.sql_view.is_none());
    }

    #[test]
    fn parse_openapi_binding_full_read_write_project() {
        let fm = json!({
            "backend": {
                "kind": "openapi",
                "endpoint": "crm_rest",
                "read":  { "operationId": "getCustomer", "path": "/customers/{id}" },
                "write": { "method": "patch", "path": "/customers/{id}" },
                "project": { "display_name": "$.name", "tier": "$.account_tier" }
            }
        });
        let b = BackendBinding::parse(&fm);
        assert_eq!(b.kind, BackendKind::OpenApi);
        let r = b.remote.expect("remote binding present");
        assert_eq!(r.kind, RemoteKind::OpenApi);
        assert_eq!(r.endpoint, "crm_rest");
        // read defaults to GET
        assert_eq!(
            r.read,
            RemoteOp::Http {
                method: "GET".into(),
                path: "/customers/{id}".into()
            }
        );
        // write method is upper-cased
        assert_eq!(
            r.write,
            Some(RemoteOp::Http {
                method: "PATCH".into(),
                path: "/customers/{id}".into()
            })
        );
        assert_eq!(
            r.project.get("display_name").map(String::as_str),
            Some("$.name")
        );
    }

    #[test]
    fn parse_mcp_binding_resource_read_only() {
        let fm = json!({
            "backend": {
                "kind": "mcp",
                "endpoint": "upstream_kb",
                "read": { "resource": "kb://article/{id}" },
                "project": { "title": "$.title" }
            }
        });
        let b = BackendBinding::parse(&fm);
        assert_eq!(b.kind, BackendKind::Mcp);
        let r = b.remote.expect("remote binding present");
        assert_eq!(r.kind, RemoteKind::Mcp);
        assert_eq!(
            r.read,
            RemoteOp::McpResource {
                uri: "kb://article/{id}".into()
            }
        );
        assert!(r.write.is_none(), "no write op ⇒ read-only");
    }

    #[test]
    fn parse_mcp_binding_tool_read_and_write() {
        let fm = json!({
            "backend": {
                "kind": "mcp",
                "endpoint": "upstream_kb",
                "read":  { "tool": "getArticle" },
                "write": { "tool": "putArticle" }
            }
        });
        let r = BackendBinding::parse(&fm).remote.expect("remote binding");
        assert_eq!(
            r.read,
            RemoteOp::McpTool {
                name: "getArticle".into()
            }
        );
        assert_eq!(
            r.write,
            Some(RemoteOp::McpTool {
                name: "putArticle".into()
            })
        );
    }

    #[test]
    fn parse_remote_missing_endpoint_keeps_kind_but_no_binding() {
        // kind present but endpoint absent → fail-closed at create, not here.
        let fm = json!({ "backend": { "kind": "openapi", "read": { "path": "/x" } } });
        let b = BackendBinding::parse(&fm);
        assert_eq!(b.kind, BackendKind::OpenApi);
        assert!(b.remote.is_none());
    }

    #[test]
    fn remote_kinds_are_read_only_page_grain_no_search_lane() {
        // The capability contract the wire surface reports for remote backends.
        for kind in [BackendKind::OpenApi, BackendKind::Mcp] {
            let c = super::super::Capabilities::for_kind(kind);
            assert!(!c.writable, "remote overlay is not update_page-writable");
            assert!(!c.supports_crdt, "remote body is not CRDT-co-authored");
            assert_eq!(c.search, super::super::SearchMode::None);
            assert!(kind.is_remote());
        }
    }
}
