# Downstream-app integration contract

**Status:** Proposal. Locked items move into the table in [`README.md`](README.md#locked-design-decisions); open items live at the bottom of this file.
**Scope:** The contract escurel commits to for *applications built on top of escurel* — specifically, what their integration test harness can rely on. The rest of the spec describes the service from the operator's and implementer's seat; this doc describes it from the seat of someone wiring escurel into another product's tests.

The motivating shape is concrete: a new application — frontend + backend — that uses escurel as its store and chains through triton (the DataZoo agent-ingress gateway) to its agents. The integration test the application's harness needs to write is:

```
escurel → app-backend → triton → app-frontend
```

Today that test costs ~300 lines of bespoke fixture, JSON-RPC, and wiremock plumbing copied out of escurel's own crate tests. This doc closes that gap.

## What this doc is not

- Not a tutorial. The getting-started narrative lives in each consuming app's README, not here.
- Not a guarantee about escurel's *internal* APIs. The contract is the public surface listed below; internals (`Indexer`, `LaneStore`, the markdown parser) may change.
- Not a re-spec of the wire protocol. [`protocol.md`](protocol.md) remains the source of truth; this doc references it.

## Why this is in the spec at all

Escurel earns its keep when applications ship on top of it. If every new application has to reverse-engineer a working test harness from `crates/escurel-server/tests/`, the friction is enough to discourage applications from landing — and the v1 cut-line in [`README.md`](README.md#what-v1-ships) gets met but unused. The contract below is small, but its absence is load-bearing.

## Gaps this contract closes

The escurel workspace already contains the *primitives* a downstream test needs; what is missing is the *façade* that hides the plumbing.

| Need (downstream test) | Today | Contract commitment |
|---|---|---|
| Spawn escurel on a random port | `serve(ServerConfig::test_defaults())` in [`crates/escurel-server/src/server.rs`](../../crates/escurel-server/src/server.rs) returns a `ServerHandle` with `local_addr` — already correct shape. | Stable. Stays. |
| Reusable process façade | Local `Harness` struct in [`crates/escurel-server/tests/mcp.rs`](../../crates/escurel-server/tests/mcp.rs) — copied per test file. | New `escurel-test-support` crate exposes `EscurelProcess`. |
| Seed pages/skills/instances | Hand-written markdown strings + `update_page` loop in `tests/mcp.rs` (`start_with_seeded_indexer`). | `FixtureBuilder` chainable seeder in `escurel-test-support`. |
| OIDC in tests | RSA keygen + wiremock JWKS dance in [`crates/escurel-server/tests/auth_quota.rs`](../../crates/escurel-server/tests/auth_quota.rs) (`keys`, `jwks_mock`, `token`). | `AuthMode::TestIssuer` runs an in-process JWKS+signer; `process.mint_token(...)` is the only call a test makes. |
| Typed client for the app's *backend* | `escurel-proto` exists and is consumed server-side (the gateway in `crates/escurel-server/src/server.rs` wires `EscurelServer` from the tonic codegen), but no `escurel-client` crate exists yet. | New `escurel-client` crate built on `escurel-proto`. |
| Typed MCP test client | Raw JSON-RPC `POST /mcp` in `tests/mcp.rs` (`call_tool`). | `McpTestClient` in `escurel-test-support`, wrapping `escurel-client`. |
| Recipe for `escurel + X` chaining | Not present. | §"Chaining recipe" below. |

The implementation of `escurel-test-support` and `escurel-client` is a separate milestone (see §"Implementation status"). This doc fixes the *contract* so the implementing PRs and the first consuming application can land in parallel.

## Test-process façade

A downstream test imports one crate (`escurel-test-support` as a `dev-dependency`) and uses one type to bring escurel up. The contract is:

```rust
// not yet implemented; this is the committed shape.

pub struct EscurelProcess { /* opaque */ }

pub struct Opts {
    pub auth: AuthMode,
    pub fixtures: Option<FixtureBuilder>,
    pub config_overrides: ConfigOverrides,
}

impl EscurelProcess {
    pub async fn spawn(opts: Opts) -> Self;

    pub fn base_url(&self) -> &str;       // http://127.0.0.1:<random>
    pub fn mcp_url(&self) -> String;      // base_url + "/mcp"

    pub fn mint_token(&self, tenant: &str, role: Role) -> String;

    pub fn client(&self) -> escurel_client::Client;        // typed RPC
    pub fn mcp_client(&self) -> McpTestClient;             // typed JSON-RPC

    pub async fn shutdown(self);
}
```

Invariants the contract commits to:

1. **No external dependencies.** No docker, no testcontainers, no network. The process binds `127.0.0.1:0`, uses a `tempfile::TempDir` for `${ESCUREL_DATA_DIR}`, and tears down on `shutdown()` or `Drop`.
2. **Parallel-safe.** Concurrent `EscurelProcess::spawn` calls in `cargo test` (each test gets its own port + temp dir).
3. **No race on bind.** `spawn` returns only once the HTTP listener is bound, exactly like `serve()` does today ([`crates/escurel-server/src/server.rs`](../../crates/escurel-server/src/server.rs) — bind happens before `ServerHandle` is returned).
4. **No global state.** Multiple instances co-exist in one process.

The precedent is triton's `TritonProcess::spawn_with_env` in [`triton/crates/triton-tests/src/lib.rs`](../../../triton/crates/triton-tests/src/lib.rs); `EscurelProcess` matches its shape so a harness running both reads as one idiom.

## Auth in tests

The current escurel server's `ServerConfig` makes OIDC optional (`verifier: Option<Arc<OidcVerifier>>` in [`crates/escurel-server/src/server.rs`](../../crates/escurel-server/src/server.rs)). That's the right primitive; the contract layers a single enum on top so tests don't choose between "no auth" and "120 lines of RSA + wiremock".

```rust
pub enum AuthMode {
    /// No verifier installed. /mcp is unauthenticated.
    Disabled,

    /// EscurelProcess stands up an in-process JWKS endpoint with
    /// an ephemeral Ed25519 or RSA keypair. `mint_token(...)` signs
    /// JWTs that the running server will accept.
    TestIssuer,

    /// Point at a real OIDC. Used when the application's tests want
    /// to exercise the production auth path end-to-end.
    External { issuer_url: String, jwks_url: String },
}
```

Commitments:

- A test using `AuthMode::TestIssuer` never imports `wiremock`, `jsonwebtoken`, or `rsa` directly.
- `process.mint_token(tenant, role)` is the only call a test makes to get a bearer token. Roles cover the `admin` claim from [`platform.md`](platform.md#auth).
- The TestIssuer's keypair lives only in the `EscurelProcess`; it is regenerated per spawn.

The implementation reuses the JWKS+RSA helpers from `crates/escurel-server/tests/auth_quota.rs` (`keys`, `jwks_mock`, `token`); the contract just promises they get hoisted into the support crate.

## Client crate for the app's backend

The application's *backend* depends on `escurel-client`, not on `escurel-test-support`. The client crate's contract:

```rust
pub struct Client { /* opaque */ }

impl Client {
    pub async fn connect(endpoint: &str, token: SecretString) -> Result<Self, Error>;

    // typed methods mirror the MCP tool surface from protocol.md and the
    // gRPC service in crates/escurel-proto/proto/escurel.proto:
    pub async fn search(&self, req: SearchRequest) -> Result<SearchResponse, Error>;
    pub async fn resolve(&self, req: ResolveRequest) -> Result<ResolveResponse, Error>;
    pub async fn expand(&self, req: ExpandRequest) -> Result<ExpandResponse, Error>;
    pub async fn neighbours(&self, req: NeighboursRequest) -> Result<NeighboursResponse, Error>;
    pub async fn list_skills(&self, req: ListSkillsRequest) -> Result<ListSkillsResponse, Error>;
    pub async fn list_instances(&self, req: ListInstancesRequest) -> Result<ListInstancesResponse, Error>;
    pub async fn run_stored_query(&self, req: RunStoredQueryRequest) -> Result<RunStoredQueryResponse, Error>;
    pub async fn update_page(&self, req: UpdatePageRequest) -> Result<UpdatePageResponse, Error>;
    // ... live-mode + admin surface follow once protocol.md catches up.
}
```

Commitments:

1. **Built on `escurel-proto`.** Types are re-exported from the tonic codegen so the wire format and the client never drift. Adding a new MCP tool means: add to `protocol.md` → add to `escurel.proto` → tonic regenerates → typed method appears in `Client`.
2. **Transport-agnostic at the surface.** The contract is the method signatures; whether `Client` speaks MCP-over-HTTP or native gRPC under the hood is configurable, with HTTP as the default. (`protocol.md` already promises both transports carry the same surface — decision 6 in [`README.md`](README.md#locked-design-decisions).)
3. **Semver-tracked.** Breaking changes to `Client` method signatures bump escurel's minor; additions are patch-safe. Versioning is tied to escurel's release, not to the proto crate's internal version.
4. **No `escurel-server` dep.** The application's binary must not transitively pull in DuckDB or candle. `escurel-client` is a leaf crate.

`McpTestClient` in `escurel-test-support` is `Client` plus the test-only spawn glue; it is not a parallel surface.

## Fixture/seeding façade

```rust
pub struct FixtureBuilder { /* opaque */ }

impl FixtureBuilder {
    pub fn new() -> Self;

    pub fn tenant(self, id: &str) -> TenantFixture;
}

impl TenantFixture {
    pub fn skill(self, id: &str, body: impl Into<MarkdownBody>) -> Self;
    pub fn instance(self, skill: &str, id: &str, body: impl Into<MarkdownBody>) -> Self;
    pub fn page(self, path: &str, body: impl Into<MarkdownBody>) -> Self;  // escape hatch
    pub fn done(self) -> FixtureBuilder;
}
```

`Opts::fixtures` takes the builder; `spawn` applies it after the server is up by calling `update_page` (the same path `start_with_seeded_indexer` in `crates/escurel-server/tests/mcp.rs` uses today). Commitment: a fixture call never bypasses the public write path — what tests seed is what `update_page` would seed in production. This keeps the seeding surface honest as the indexer evolves.

## MCP test client

The current tests hand-craft JSON-RPC 2.0 envelopes (`call_tool` in `crates/escurel-server/tests/mcp.rs`). The contract replaces that with:

```rust
let resp: SearchResponse = process.mcp_client()
    .search(SearchRequest { query: "acme".into(), top_k: 5, ..Default::default() })
    .await?;
```

This is mechanically the same as `Client::search`; the difference is that `McpTestClient` is constructed from the spawned process and pre-loaded with `mint_token(...)` so the test doesn't manage tokens. Tests that *want* to manage tokens explicitly use `process.client()` instead.

## Chaining recipe — `escurel → app-backend → triton → app-frontend`

The contract above lets the application's integration test read like this:

```rust
// in the consuming app's `tests/e2e.rs`

#[tokio::test]
async fn dashboard_round_trips_through_triton() {
    // 1. escurel up, with a tenant seeded.
    let escurel = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new()
            .tenant("acme")
                .skill("customer", include_str!("fixtures/customer.skill.md"))
                .instance("customer", "acme-corp", include_str!("fixtures/acme-corp.md"))
                .done()),
        ..Default::default()
    }).await;
    let token = escurel.mint_token("acme", Role::User);

    // 2. app backend up, pointed at escurel.
    let backend = MyAppBackend::spawn(BackendOpts {
        escurel_url: escurel.base_url().into(),
        escurel_token: token.clone(),
    }).await;

    // 3. triton up, with FakeConsul pointing one tool at the app backend.
    //    (Triton's existing harness — TritonProcess + upstream_fixture —
    //     covers this; see triton/crates/triton-tests/src/upstream_fixture.rs,
    //     the FakeConsul fixture.)
    let triton = TritonProcess::spawn_with_env(
        Duration::from_secs(5),
        fake_consul_pointing_at(&backend.url()),
    ).await;

    // 4. drive the frontend (chrome-devtools MCP, playwright, or a thin
    //    HTTP driver — the app picks). Assert end-to-end.
    let frontend = MyAppFrontend::new(triton.rest_url("/v1/tools/dashboard"));
    let result = frontend.render_dashboard("acme-corp").await?;
    assert!(result.contains("Acme Corp"));

    // 5. (optional) audit-chain assertion: triton's harness already
    //    captures audit lines by trace_id; assert the inbound and
    //    upstream lines share one id, the way the dispatcher-pair
    //    assertions in triton/crates/triton-tests/tests/upstream.rs do.
}
```

Commitments specific to chaining:

- The contract does **not** require triton to know about escurel; the application's backend is the integration point. (Triton's tree has zero references to escurel today and this contract keeps it that way.)
- `trace_id` / `request_id` propagation is the application's responsibility on its own HTTP path; escurel and triton each preserve incoming `request_id` headers per their own observability sections ([`platform.md`](platform.md#observability) and triton's audit pipeline).
- The application's frontend driver is out of scope. The integration-test contract ends at HTTP boundaries; how the application drives its UI is the application's choice.

## Stability and versioning

What this contract guarantees across escurel versions:

- `EscurelProcess` method signatures (`spawn`, `base_url`, `mcp_url`, `mint_token`, `client`, `mcp_client`, `shutdown`) are semver-stable.
- `AuthMode` variant set is semver-stable (adding variants is breaking; renaming is breaking).
- `Client`'s typed methods track [`protocol.md`](protocol.md) one-to-one; method additions are patch-safe, signature changes are breaking.
- `FixtureBuilder` builds against the public write path (`update_page`); semantic equivalence with that path is the contract, not the builder's surface ergonomics.

What it does **not** guarantee:

- `FixtureBuilder` method names (these may evolve as new fixture kinds are added).
- The shape of `ConfigOverrides` (it expands as new server knobs land — additive only).
- Internal types: `Indexer`, `LaneStore`, `OidcVerifier`. Tests that reach for those import `escurel-server` directly and accept the churn.

## Implementation status

Not yet implemented. This document fixes the contract; the implementing milestone delivers:

1. **`crates/escurel-test-support/`** — `EscurelProcess`, `Opts`, `AuthMode`, `FixtureBuilder`, `McpTestClient`. Reuses the helpers already in `tests/auth_quota.rs` and `tests/mcp.rs`.
2. **`crates/escurel-client/`** — typed wrapper around `escurel-proto`'s tonic codegen, with HTTP and gRPC transports.
3. **`examples/echo-app/`** (or a sibling repo) — a minimal application demonstrating the full chaining recipe above, with its `tests/e2e.rs` as the executable proof that the contract holds.

The order is `escurel-client` → `escurel-test-support` (which depends on it) → example app. The example app's `tests/e2e.rs` is the acceptance test for this contract: if it does not read roughly like the §"Chaining recipe" snippet above, the contract has drifted from the implementation and one of them needs to move.

## Open questions

- **gRPC vs HTTP default for `Client`.** Both transports are committed (decision 6 in [`README.md`](README.md#locked-design-decisions)). The contract does not yet pick a default; the implementing PR should pick whichever has the smaller dependency footprint for downstream apps (likely HTTP) and document the choice.
- **`escurel-test-support` published or workspace-only.** During bootstrap (CI paused per [`CLAUDE.md`](../../CLAUDE.md) principle 2), workspace-only is fine. Before v1 stable, decide whether it is published to a registry; if not, downstream apps that live in other repos consume it as a git dependency.
- **Multi-tenant fixtures.** `FixtureBuilder::tenant(id)` chains today; whether one `FixtureBuilder` may declare two tenants in one call is left to the implementing PR — neither shape changes the contract's commitments.
