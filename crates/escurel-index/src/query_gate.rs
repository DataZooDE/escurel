//! The gate on authored query SQL (#854).
//!
//! `query_instance` authorises a caller to read ONE target instance, then runs
//! the page's SQL on the indexer's own connection. That connection also holds
//! `external_credentials` (plaintext DSNs), every other instance's `vw_` view,
//! `pages`, the search projection, and DuckDB's file and catalog readers. The
//! ACL check is about the target, so the SQL may read nothing else.
//!
//! The rule is **default-deny over DuckDB's own parse tree**
//! (`json_serialize_sql`, the parser that will run the statement):
//!
//! - exactly one `SELECT` statement (`COPY`, DML, DDL, `SET`, `ATTACH`, `PRAGMA`
//!   and multi-statement text do not serialise, so they are refused);
//! - every table reference is a join, a subquery, `VALUES`, an empty `FROM`, a
//!   pure generator (`range`, `generate_series`, `unnest`), or an unqualified
//!   name that is the target view or a CTE **in scope at that point**. Any other
//!   reference kind, including one this code has never seen, is refused;
//! - no function that reads process or engine state (`getenv`,
//!   `current_setting`, the `duckdb_*` / `read_*` / `pragma_*` families, …) and
//!   no catalog- or schema-qualified function.
//!
//! Scoping matters: a CTE named `pages` inside one subquery must not let a
//! sibling `FROM pages` through, so CTE names are visible only below the query
//! node that declares them.

use duckdb::Connection;
use serde_json::Value;

/// Table functions allowed in `FROM`: pure generators that read no state.
const GENERATORS: &[&str] = &["range", "generate_series", "unnest"];

/// Table-reference kinds that are allowed in themselves (their names and
/// children are still checked).
const ALLOWED_REFS: &[&str] = &[
    "BASE_TABLE",
    "JOIN",
    "SUBQUERY",
    "TABLE_FUNCTION",
    "EXPRESSION_LIST",
    "EMPTY",
    "EMPTY_FROM",
];

/// Node kinds that are not table references: query nodes and result modifiers.
const QUERY_NODES: &[&str] = &[
    "SELECT_NODE",
    "SET_OPERATION_NODE",
    "RECURSIVE_CTE_NODE",
    "CTE_NODE",
    "ORDER_MODIFIER",
    "LIMIT_MODIFIER",
    "LIMIT_PERCENT_MODIFIER",
    "DISTINCT_MODIFIER",
];

/// Scalar functions that read process, session or engine state.
const DENIED_FUNCTIONS: &[&str] = &[
    "getenv",
    "current_setting",
    "getvariable",
    "query",
    "query_table",
    "glob",
    "sniff_csv",
    "which_secret",
    "current_database",
    "current_schema",
    "current_schemas",
    "current_query",
];

/// Function-name families that read files, the catalog or attached sources.
const DENIED_PREFIXES: &[&str] = &[
    "duckdb_",
    "pragma_",
    "read_",
    "parquet_",
    "ducklake_",
    "iceberg_",
    "delta_",
    "postgres_",
    "mysql_",
    "sqlite_",
    "http",
];

/// Refuse `sql` unless it is a single SELECT that reads only `target_view`, its
/// own CTEs and generators. `Err` is the reason, for the caller's error.
pub(crate) fn check(conn: &Connection, sql: &str, target_view: &str) -> Result<(), String> {
    let serialised: String = conn
        .query_row("SELECT json_serialize_sql(?::VARCHAR)", [sql], |r| r.get(0))
        .map_err(|e| format!("the statement could not be parsed: {e}"))?;
    let tree: Value = serde_json::from_str(&serialised)
        .map_err(|e| format!("the parse tree could not be read: {e}"))?;
    if tree.get("error").and_then(Value::as_bool) != Some(false) {
        let msg = tree
            .get("error_message")
            .and_then(Value::as_str)
            .unwrap_or("not a SELECT statement");
        return Err(format!("only a single SELECT statement is allowed ({msg})"));
    }
    let statements = tree
        .get("statements")
        .and_then(Value::as_array)
        .ok_or("the parse tree has no statements")?;
    if statements.len() != 1 {
        return Err("only a single SELECT statement is allowed".to_owned());
    }
    let gate = Gate {
        target: target_view.to_ascii_lowercase(),
    };
    gate.walk(&statements[0], &[])
}

struct Gate {
    target: String,
}

impl Gate {
    fn walk(&self, node: &Value, scope: &[String]) -> Result<(), String> {
        match node {
            Value::Array(items) => items.iter().try_for_each(|v| self.walk(v, scope)),
            Value::Object(map) => {
                // A query node's CTEs are visible to everything below it,
                // including the CTE bodies (recursive CTEs name themselves).
                let mut inner: Vec<String>;
                let mut scope = scope;
                if let Some(ctes) = map
                    .get("cte_map")
                    .and_then(|c| c.get("map"))
                    .and_then(Value::as_array)
                    && !ctes.is_empty()
                {
                    inner = scope.to_vec();
                    inner.extend(
                        ctes.iter()
                            .filter_map(|e| e.get("key").and_then(Value::as_str))
                            .map(str::to_ascii_lowercase),
                    );
                    scope = &inner;
                }

                let kind = map.get("type").and_then(Value::as_str);
                let class = map.get("class").and_then(Value::as_str);
                match (kind, class) {
                    // An expression.
                    (_, Some("FUNCTION")) => self.scalar_function(map)?,
                    (_, Some(_)) => {}
                    // A table reference (it has an alias and a location, and
                    // no expression class), or a query node / modifier.
                    (Some(kind), None) => {
                        if QUERY_NODES.contains(&kind) {
                        } else if ALLOWED_REFS.contains(&kind) {
                            match kind {
                                "BASE_TABLE" => self.base_table(map, scope)?,
                                "TABLE_FUNCTION" => {
                                    self.table_function(map)?;
                                    // Its arguments may hold subqueries: walk
                                    // them, but not the generator itself as a
                                    // scalar call.
                                    if let Some(args) =
                                        map.get("function").and_then(|f| f.get("children"))
                                    {
                                        self.walk(args, scope)?;
                                    }
                                    return self.walk_except(map, &["function"], scope);
                                }
                                _ => {}
                            }
                        } else if map.contains_key("alias") && map.contains_key("query_location") {
                            return Err(format!("a {kind} table reference is not allowed"));
                        }
                    }
                    (None, None) => {}
                }
                self.walk_except(map, &[], scope)
            }
            _ => Ok(()),
        }
    }

    fn walk_except(
        &self,
        map: &serde_json::Map<String, Value>,
        skip: &[&str],
        scope: &[String],
    ) -> Result<(), String> {
        map.iter()
            .filter(|(k, _)| !skip.contains(&k.as_str()))
            .try_for_each(|(_, v)| self.walk(v, scope))
    }

    fn base_table(
        &self,
        map: &serde_json::Map<String, Value>,
        scope: &[String],
    ) -> Result<(), String> {
        let name = map.get("table_name").and_then(Value::as_str).unwrap_or("");
        let qualified = ["schema_name", "catalog_name"].iter().any(|k| {
            map.get(*k)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        });
        if qualified {
            return Err(format!("{name:?} is qualified; read {{{{target}}}} only"));
        }
        if map.get("at_clause").is_some_and(|a| !a.is_null()) {
            return Err(format!(
                "{name:?} uses AT (time travel), which is not allowed"
            ));
        }
        let lower = name.to_ascii_lowercase();
        if lower == self.target || scope.contains(&lower) {
            Ok(())
        } else {
            Err(format!(
                "it reads {name:?}; a query may read only {{{{target}}}} and its own CTEs"
            ))
        }
    }

    fn table_function(&self, map: &serde_json::Map<String, Value>) -> Result<(), String> {
        let f = map.get("function");
        let name = f
            .and_then(|f| f.get("function_name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if f.is_some_and(qualified_function) || !GENERATORS.contains(&name.as_str()) {
            return Err(format!(
                "table function {name:?} is not allowed (only {})",
                GENERATORS.join(", ")
            ));
        }
        Ok(())
    }

    fn scalar_function(&self, map: &serde_json::Map<String, Value>) -> Result<(), String> {
        let name = map
            .get("function_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let node = Value::Object(map.clone());
        if qualified_function(&node) {
            return Err(format!("qualified function {name:?} is not allowed"));
        }
        if DENIED_FUNCTIONS.contains(&name.as_str())
            || DENIED_PREFIXES.iter().any(|p| name.starts_with(p))
        {
            return Err(format!(
                "function {name:?} reads server state and is not allowed"
            ));
        }
        Ok(())
    }
}

/// A function qualified with anything but the built-in `system.main` (which
/// the parser itself writes for desugared calls such as a list literal's
/// `list_value`): such a call could reach an attached catalog's macro.
fn qualified_function(f: &Value) -> bool {
    let part = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("");
    !matches!(part("schema"), "" | "main") || !matches!(part("catalog"), "" | "system")
}
