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
        match block.get("kind").and_then(serde_json::Value::as_str) {
            Some("markdown") | None => Self::default(),
            Some("sql_view") => Self {
                kind: BackendKind::SqlView,
                sql_view: parse_sql_view(block),
            },
            // Unknown kind: lenient on the read path (markdown); the
            // create/validate path is where a bad binding is rejected.
            Some(_) => Self::default(),
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
}
