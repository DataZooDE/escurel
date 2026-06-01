# 06 — Integration tests (the no-mock dev loop)

Escurel's own discipline is **red→green TDD with no mocks at the boundary**
(`CLAUDE.md` principles 1–2): a task is done when a no-mock integration
test passes against the *real* component. Your app inherits this. The
support crate makes it a few lines instead of ~300 of fixture plumbing.
Contract: `docs/spec/dx.md`. Live proof: `examples/echo-app/tests/e2e.rs`.

## Rust: `escurel-test-support`

A dev-dependency that spawns a **real, in-process** gateway — no docker, no
testcontainers, no network, no compiled server binary needed. It binds
`127.0.0.1:0` and uses a `tempfile` data dir, so tests run in parallel and
clean up on `Drop`.

```toml
[dev-dependencies]
escurel-test-support = { path = "../escurel/crates/escurel-test-support" }
tokio    = { version = "1", features = ["macros", "rt-multi-thread"] }
reqwest  = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

The full surface (`crates/escurel-test-support/src/`):

| Item | Use |
|---|---|
| `EscurelProcess::spawn(Opts) -> EscurelProcess` | async; returns only once the listener is bound (no bind race) |
| `Opts { auth, fixtures, config_overrides }` | `Default` = no-auth, no-fixtures, default version |
| `AuthMode::{Disabled, TestIssuer, External{issuer_url, jwks_url}}` | `TestIssuer` runs an in-process JWKS+signer (`references/08`) |
| `Role::{Agent, Admin}` | role for a minted token |
| `FixtureBuilder` / `TenantFixture` | chainable seeding (`references/07`) |
| `ConfigOverrides { … }` | optional knobs (quota, readiness probe, crdt backend, …) |
| `.base_url()` / `.mcp_url()` / `.ws_url()` | HTTP base / `/mcp` / `/ws` URLs (point your backend's `escurel-client` at `base_url()`) |
| `.mint_token(tenant, role) -> String` | bearer; **`TestIssuer` only** (panics otherwise) |
| `.client() -> Client` | sync; typed `escurel-client` (HTTP MCP) pre-tokened for the default `acme` tenant |
| `.client_for(tenant, role) -> Client` | async; client for any tenant/role |
| `.mcp_client() -> McpTestClient` | typed MCP-over-HTTP client (same methods, returns the same response types) |
| `.shutdown()` | async; explicit graceful teardown (else `Drop` handles it) |

### The canonical test (from `examples/echo-app/tests/e2e.rs`)

```rust
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};

#[tokio::test]
async fn round_trips() {
    // 1. escurel up, tenant seeded through the public write path.
    let escurel = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                    .skill("customer", include_str!("fixtures/customer.skill.md"))
                    .instance("customer", "acme-corp", include_str!("fixtures/acme-corp.md"))
                    .done(),
        ),
        ..Default::default()
    }).await;
    let token = escurel.mint_token("acme", Role::Agent);

    // 2. your backend up, pointed at escurel's HTTP base URL.
    let backend = my_app::spawn(my_app::Opts {
        escurel_endpoint: escurel.base_url().to_owned(),
        escurel_token: token,
    }).await.unwrap();

    // 3. drive your app's HTTP edge; assert the round-trip.
    let resp = reqwest::Client::new()
        .get(format!("{}/pages/acme-corp", backend.base_url()))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("Acme Corp"));

    backend.shutdown().await;
    escurel.shutdown().await;
}
```

Run it: `cargo test -p <your-crate>`. Test the unhappy paths too — the
example's `unknown_slug_returns_404` spawns with no fixtures and asserts a
missing wikilink becomes a 404, not a 5xx.

### Driving Escurel directly in a test

When you want to assert against Escurel without your backend in the loop,
use the pre-tokened clients straight off the process:

```rust
let hits = escurel.client().search(SearchRequest { q: "acme".into(), ..Default::default() }).await?;
// or over MCP-over-HTTP, identical response type:
let hits = escurel.mcp_client().search(SearchRequest { q: "acme".into(), ..Default::default() }).await?;
```

## Non-Rust apps

`escurel-test-support` is Rust-only. For a Python/TS/Go app:

1. Stand a gateway up (`references/09`) — for CI, the simplest reliable
   path is a tiny Rust helper crate that calls `EscurelProcess::spawn`,
   prints `base_url()`/`mcp_url()` + a minted token, and stays up
   until killed; your test harness shells out to it. Or point at a shared
   dev/`nonprod` instance.
2. Seed via the public write path: loop `escurel update-page <path> < body`
   (`references/04`), or POST `update_page` over `/mcp` (`references/03`).
3. Drive your app and assert the round-trip — same shape as above, just
   in your language's test runner.

The invariant that matters in every language: **no mocks at the Escurel
boundary** — the test exercises the real gateway and real seeded data.
