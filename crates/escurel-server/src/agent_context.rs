//! escurel → agent tabular data plane, native (fleet epic #801 Phase 4, the
//! `anofox-context` direction).
//!
//! The alternative to the quack RPC data plane (where the agent connects INTO
//! escurel and hit the cross-tenant boundary problem). Here escurel is the
//! caller: it runs the requester's *entitled* SELECT (its own Rust ACLs build
//! that query), serializes the rows, and POSTs them to the agent as an
//! OpenAI-compatible endpoint with a prompt + an output schema; the agent
//! returns a typed table that lands back in escurel.
//!
//! This is `anofox_context::context_query` done natively in escurel rather than
//! as a loaded DuckDB extension — deliberately, for two reasons: (1) the
//! extension is built per-DuckDB-patch (`v1.5.4`) and will not load in escurel's
//! `v1.5.5` (the same gap as quack-oauth); (2) a SQL-callable HTTP client on the
//! gateway connection is an exfil surface, whereas here escurel controls the
//! endpoint (a single allowlisted agent) and the SQL, so nothing untrusted can
//! reach it.
//!
//! Why this dissolves the quack blockers: the agent NEVER connects to escurel's
//! instance, so there is no served connection, no client attach, no
//! parse-time policy walk to under-approximate (crew F1), and no
//! attach-time auth to get wrong (F7). escurel sends exactly the pre-ACL-filtered
//! rows it chose. Containment by construction.
//!
//! Fit: steps whose work is LLM-reasoning over tabular data (classify / extract
//! / enrich / propose). Heavy DuckDB compute (scenario freeze, evolve) is a
//! different seam (the A2A control channel + the #291 producer). Input is
//! bounded (a serialized-row cap), so this is for small/medium context, not
//! large-result transport.

use duckdb::Connection;
use serde_json::{Value, json};

/// Why a `context_query` could not complete.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// Running the entitled `data_query` failed.
    #[error("context_query: data query failed: {0}")]
    DataQuery(String),
    /// The serialized input exceeded the byte cap.
    #[error("context_query: input {got} bytes exceeds the {cap}-byte cap")]
    TooLarge {
        /// Actual serialized size.
        got: usize,
        /// The configured cap.
        cap: usize,
    },
    /// The agent endpoint call failed (transport / non-2xx).
    #[error("context_query: agent endpoint: {0}")]
    Endpoint(String),
    /// The agent's response was not the expected chat-completion / typed-table
    /// shape.
    #[error("context_query: agent response: {0}")]
    BadResponse(String),
}

/// A column of the requested output schema: `(name, duckdb_type)`.
pub type SchemaCol = (String, String);

/// The default cap on serialized input bytes (mirrors `anofox_context`'s
/// `context_max_data_bytes`) — this is a reasoning seam, not bulk transport.
pub const DEFAULT_MAX_DATA_BYTES: usize = 100 * 1024;

/// Parameters for one `context_query`.
pub struct ContextQuery<'a> {
    /// The requester's ENTITLED SELECT — built by escurel's own ACLs. Its rows
    /// are the only data the agent ever sees.
    pub data_query: &'a str,
    /// Instructions to the agent.
    pub prompt: &'a str,
    /// The columns (name + type) the agent must return.
    pub output_schema: &'a [SchemaCol],
    /// The agent's OpenAI-compatible endpoint (allowlisted; the single trusted
    /// callee).
    pub endpoint: &'a str,
    /// The model name.
    pub model: &'a str,
    /// The bearer presented to the agent (the delegation token).
    pub bearer: &'a str,
    /// Serialized-input byte cap.
    pub max_data_bytes: usize,
}

/// Serialize the entitled query's rows to a JSON array using DuckDB's own
/// `to_json` (no generic Rust type-handling; DuckDB renders each row).
fn serialize_rows(conn: &Connection, data_query: &str) -> Result<Vec<Value>, ContextError> {
    // Wrap the caller's SELECT; `to_json(t)` renders each row as one JSON object.
    let sql = format!("SELECT to_json(t) AS j FROM ({data_query}) AS t");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| ContextError::DataQuery(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| ContextError::DataQuery(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        let s = r.map_err(|e| ContextError::DataQuery(e.to_string()))?;
        let v: Value = serde_json::from_str(&s)
            .map_err(|e| ContextError::DataQuery(format!("to_json parse: {e}")))?;
        out.push(v);
    }
    Ok(out)
}

/// The system prompt: the instructions + a strict "return a JSON array matching
/// this schema" contract, so the typed table drops straight back into escurel.
fn system_prompt(prompt: &str, schema: &[SchemaCol]) -> String {
    let cols: Vec<String> = schema.iter().map(|(n, t)| format!("{n} ({t})")).collect();
    format!(
        "{prompt}\n\nReturn ONLY a JSON array of objects, each with exactly these \
         columns: {}. No prose, no markdown fences — just the JSON array.",
        cols.join(", ")
    )
}

/// Run one escurel→agent `context_query`: entitled SELECT → serialize → POST to
/// the agent → parse the typed table.
///
/// # Errors
/// [`ContextError`] on a data-query failure, an over-cap input, an endpoint
/// error, or an unparseable response.
pub async fn context_query(
    conn: &Connection,
    http: &reqwest::Client,
    q: &ContextQuery<'_>,
) -> Result<Vec<Value>, ContextError> {
    let rows = serialize_rows(conn, q.data_query)?;
    let rows_json = Value::Array(rows).to_string();
    if rows_json.len() > q.max_data_bytes {
        return Err(ContextError::TooLarge {
            got: rows_json.len(),
            cap: q.max_data_bytes,
        });
    }

    let body = json!({
        "model": q.model,
        "messages": [
            { "role": "system", "content": system_prompt(q.prompt, q.output_schema) },
            { "role": "user", "content": rows_json },
        ],
        "temperature": 0,
    });
    let resp = http
        .post(q.endpoint)
        .bearer_auth(q.bearer)
        .json(&body)
        .send()
        .await
        .map_err(|e| ContextError::Endpoint(format!("transport: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| ContextError::Endpoint(format!("body: {e}")))?;
    if !status.is_success() {
        return Err(ContextError::Endpoint(format!("status {status}: {text}")));
    }
    let envelope: Value = serde_json::from_str(&text)
        .map_err(|e| ContextError::BadResponse(format!("not JSON: {e}")))?;
    let content = envelope["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| ContextError::BadResponse("no choices[0].message.content".to_owned()))?;
    let parsed: Value = serde_json::from_str(content.trim())
        .map_err(|e| ContextError::BadResponse(format!("content not JSON: {e}")))?;
    let arr = parsed
        .as_array()
        .ok_or_else(|| ContextError::BadResponse("content is not a JSON array".to_owned()))?;

    // Validate every returned row carries exactly the requested columns — the
    // typed contract, so a malformed agent reply is refused rather than landed.
    for (i, row) in arr.iter().enumerate() {
        let obj = row
            .as_object()
            .ok_or_else(|| ContextError::BadResponse(format!("row {i} is not an object")))?;
        for (name, _ty) in q.output_schema {
            if !obj.contains_key(name) {
                return Err(ContextError::BadResponse(format!(
                    "row {i} missing column {name:?}"
                )));
            }
        }
    }
    Ok(arr.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub OpenAI-compatible agent endpoint: it echoes back a typed table
    /// derived from the rows escurel sent, proving escurel serialized the
    /// entitled rows + parsed the typed result. Records the last request for
    /// assertions.
    async fn spawn_stub_agent(seen: std::sync::Arc<std::sync::Mutex<Option<Value>>>) -> String {
        use axum::{Router, extract::State, routing::post};

        async fn chat(
            State(seen): State<std::sync::Arc<std::sync::Mutex<Option<Value>>>>,
            body: String,
        ) -> axum::Json<Value> {
            let req: Value = serde_json::from_str(&body).expect("agent got JSON");
            // The user message is escurel's serialized entitled rows.
            let user = req["messages"][1]["content"].as_str().unwrap_or("[]");
            let rows: Vec<Value> = serde_json::from_str(user).unwrap_or_default();
            *seen.lock().unwrap() = Some(req.clone());
            // Return a typed table: {id, verdict} per input row.
            let out: Vec<Value> = rows
                .iter()
                .map(|r| json!({ "id": r["id"], "verdict": "reviewed" }))
                .collect();
            let content = Value::Array(out).to_string();
            axum::Json(json!({ "choices": [{ "message": { "content": content } }] }))
        }

        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .with_state(seen);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1/chat/completions")
    }

    #[tokio::test]
    async fn escurel_calls_the_agent_with_entitled_rows_and_lands_a_typed_table() {
        // escurel's own instance with an entitled view (escurel's ACLs would
        // build this SELECT; here we seed it directly).
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE entitled AS SELECT * FROM (VALUES (1,'widgets'),(2,'gears')) t(id, item);",
        )
        .expect("seed");

        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let endpoint = spawn_stub_agent(seen.clone()).await;

        let out = context_query(
            &conn,
            &reqwest::Client::new(),
            &ContextQuery {
                data_query: "SELECT id, item FROM entitled ORDER BY id",
                prompt: "Review each item.",
                output_schema: &[
                    ("id".to_owned(), "INTEGER".to_owned()),
                    ("verdict".to_owned(), "VARCHAR".to_owned()),
                ],
                endpoint: &endpoint,
                model: "agent",
                bearer: "delegation-token",
                max_data_bytes: DEFAULT_MAX_DATA_BYTES,
            },
        )
        .await
        .expect("context_query");

        // The agent saw exactly the entitled rows escurel serialized.
        let req = seen.lock().unwrap().clone().expect("agent was called");
        let user = req["messages"][1]["content"].as_str().unwrap();
        let sent: Vec<Value> = serde_json::from_str(user).unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["item"], "widgets");
        assert_eq!(
            req["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("verdict (VARCHAR)"),
            true
        );

        // The typed table came back and matches the schema.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[0]["verdict"], "reviewed");
    }

    #[tokio::test]
    async fn a_response_missing_a_schema_column_is_refused() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch("CREATE TABLE entitled AS SELECT 1 AS id;")
            .expect("seed");

        // A stub that returns rows WITHOUT the required `verdict` column.
        use axum::{Router, routing::post};
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                axum::Json(json!({ "choices": [{ "message": { "content": "[{\"id\":1}]" } }] }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let endpoint = format!("http://{addr}/v1/chat/completions");

        let err = context_query(
            &conn,
            &reqwest::Client::new(),
            &ContextQuery {
                data_query: "SELECT id FROM entitled",
                prompt: "x",
                output_schema: &[
                    ("id".to_owned(), "INTEGER".to_owned()),
                    ("verdict".to_owned(), "VARCHAR".to_owned()),
                ],
                endpoint: &endpoint,
                model: "agent",
                bearer: "t",
                max_data_bytes: DEFAULT_MAX_DATA_BYTES,
            },
        )
        .await
        .expect_err("a row missing a required column must be refused");
        assert!(matches!(err, ContextError::BadResponse(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn over_cap_input_is_refused_before_calling_the_agent() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE entitled AS SELECT range AS id, repeat('x', 100) AS blob FROM range(1000);",
        )
        .expect("seed");
        // Endpoint deliberately bogus: if the cap check fails we'd get an
        // endpoint error instead of TooLarge.
        let err = context_query(
            &conn,
            &reqwest::Client::new(),
            &ContextQuery {
                data_query: "SELECT id, blob FROM entitled",
                prompt: "x",
                output_schema: &[("id".to_owned(), "INTEGER".to_owned())],
                endpoint: "http://127.0.0.1:1/nope",
                model: "agent",
                bearer: "t",
                max_data_bytes: 1024,
            },
        )
        .await
        .expect_err("over-cap input must be refused");
        assert!(matches!(err, ContextError::TooLarge { .. }), "got {err:?}");
    }
}
