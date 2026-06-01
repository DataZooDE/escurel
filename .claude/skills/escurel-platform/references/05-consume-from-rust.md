# 05 — Consume from Rust (`escurel-client`)

`escurel-client` (`crates/escurel-client`) is the typed MCP-over-HTTP
client a Rust **backend** depends on. It is a **leaf crate**: it pulls in
`escurel-types` (the serde wire-contract structs) and `reqwest`, and
deliberately **not** `escurel-server`, DuckDB, or candle —
your binary stays light (`references/10` makes this a hard rule). The live
reference is `examples/echo-app/src/lib.rs`.

## Depend on it

```toml
[dependencies]
escurel-client = { path = "../escurel/crates/escurel-client" }   # or git, pinned rev
secrecy = "0.10"   # for SecretString; also re-exported by escurel-client
```

## Connect

```rust
use escurel_client::{Client, SecretString};

let client = Client::connect(
    "http://127.0.0.1:8080",            // HTTP MCP endpoint
    SecretString::from(token),          // bearer; wrapped so it never logs
).await?;                               // Result<Client, escurel_client::Error>
```

- `Client` is `Clone` and cheap to clone (the underlying `reqwest` client
  is shared) — build it once at startup, wrap in `Arc`, clone per request.
- Errors: `Error::{InvalidEndpoint, InvalidToken, Transport, Http, JsonRpc}`
  — `Transport(reqwest::Error)` / `Http{status, body}` for transport-level
  failures, `JsonRpc{code, message}` for tool-level errors. The enum is
  intentionally small; additions are breaking.

## The typed methods

Each takes a `*Request` and returns a `*Response`, both re-exported from
`escurel_client` (originating in `escurel-types`):

```rust
client.search(SearchRequest { q: "acme".into(), k: 5, ..Default::default() }).await?;
client.resolve(ResolveRequest { wikilink: "[[customer::acme-corp]]".into() }).await?;
client.expand(ExpandRequest { page_id, anchor: String::new(), version: String::new() }).await?;
client.neighbours(NeighboursRequest { page_id, direction: "both".into(), ..Default::default() }).await?;
client.list_skills(ListSkillsRequest::default()).await?;
client.list_instances(ListInstancesRequest { skill: "customer".into(), ..Default::default() }).await?;
client.run_stored_query(RunStoredQueryRequest { query_id: "…".into(), params_json: "{}".into() }).await?;
client.update_page(UpdatePageRequest { page_id, content }).await?;
// Chat history (M-Chat, issue #63): append-mostly log keyed by an
// opaque chat_group_id. See `references/02` §Chat tools.
client.append_message(AppendMessageRequest {
    chat_group_id: "room-1".into(),
    role: "user".into(),
    content: "hi".into(),
    embed: true,
    ..Default::default()
}).await?;
client.list_messages(ListMessagesRequest {
    chat_group_id: "room-1".into(),
    direction: "asc".into(),
    limit: 50,
    ..Default::default()
}).await?;
```

Field names follow the wire contract (`q`/`k`, not `query`/`top_k`);
JSON-bearing fields (`frontmatter_json`, `rows_json`, `params_json`) are
JSON strings you parse. See `crates/escurel-types/src/` and
`references/03` for the message shapes. The live-CRDT trio and admin
methods are added as `protocol.md` and the types catch up.

## The backend pattern (from `examples/echo-app/src/lib.rs`)

The "chaining recipe" in one handler — resolve, then expand, then serve:

```rust
async fn get_page(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let wikilink = format!("[[customer::{slug}]]");
    let resolved = match state.escurel.resolve(ResolveRequest { wikilink }).await {
        Ok(r) => r,
        Err(e) => return upstream_error("resolve", &e.to_string()),   // → 502
    };
    if !resolved.exists {
        return (StatusCode::NOT_FOUND, …).into_response();            // → 404, not 5xx
    }
    let page = resolved.page.expect("exists ⇒ PageRef");
    let expanded = match state.escurel
        .expand(ExpandRequest { page_id: page.page_id, ..Default::default() }).await
    { Ok(e) => e, Err(e) => return upstream_error("expand", &e.to_string()) };
    expanded.body.into_response()
}
```

Translate Escurel outcomes into *your* HTTP semantics: a missing page
(`exists == false`) is a 404; an upstream call failure is a 502. Don't leak
`escurel_client::Error` to your callers.

## Configuration

Read the endpoint + token from env at startup, build `Opts`, connect once.
The example uses its own app-level env names (these are the *app's*
choice, not Escurel-defined):

```rust
// examples/echo-app/src/lib.rs — env_opts()
let escurel_endpoint = std::env::var("ESCUREL_ENDPOINT")?;   // HTTP MCP URL
let escurel_token    = std::env::var("ESCUREL_TOKEN")?;      // bearer
```

(The `escurel` CLI's own env var is `ESCUREL_SERVER`; the production server
reads `ESCUREL_SERVER_*` / `ESCUREL_AUTH_*`. Keep the three namespaces
straight — `references/09`.)

For tests, build `Opts` directly from `EscurelProcess` accessors instead of
env (so you don't poison the process env) — `references/06`.
