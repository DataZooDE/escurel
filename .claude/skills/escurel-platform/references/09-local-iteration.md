# 09 — Local iteration: getting a gateway to develop against

## The reality: no standalone `serve` binary (yet)

The workspace builds exactly **one** binary — the `escurel` CLI
(`crates/escurel-cli`), which is a *client*, not a server. `escurel-server`
is a **library** consumed by `escurel-test-support` (in-process) and, in
production, by the `escurel-server` binary in the repo's container image
(`Dockerfile` → ghcr) that the substrate Kamal deploy launches
(`docs/deploy/substrate.md`). There is
**no `escurel serve` you can run locally today.** Plan your dev loop
around that. (If this changes, this reference is the thing to update.)

So there are two practical ways to develop against a gateway:

### A. In-process via `EscurelProcess` (the default, Rust)

For a Rust app, you almost never need a separately-running gateway: your
integration tests spawn one in-process (`references/06`). This is the
fastest, most hermetic loop and matches Escurel's own no-mock discipline.
Red→green:

```sh
cargo test -p <your-crate> <test_name>     # spawns escurel + your backend, asserts
```

If you want a gateway to poke at *interactively* (e.g. to drive the CLI or
an MCP client by hand), write a throwaway binary/test that calls
`EscurelProcess::spawn`, prints `base_url()` / `mcp_url()` and a
`mint_token(...)`, and parks until Ctrl-C. Then:

```sh
export ESCUREL_SERVER=<printed base url>
export ESCUREL_TOKEN=<printed token>
escurel list-skills
```

### B. Point at a deployed instance (any language)

For non-Rust apps, or to develop against real data, point your app/CLI at
a deployed `nonprod` gateway:

```sh
export ESCUREL_SERVER="http://<host>:8080"     # CLI (HTTP MCP)
export ESCUREL_TOKEN="<bearer from the real issuer>"   # references/08
# or for your app's own client: ESCUREL_ENDPOINT / your app's bearer
```

## The routes (once a gateway is up)

| route | port | purpose |
|---|---|---|
| `POST /mcp` | 8080 | MCP-over-HTTP tool calls (`references/03`) |
| `/ws` | 8080 | live CRDT + presence |
| `/healthz` | 8080 | liveness (dependency-free) |
| `/readyz` | 8080 | readiness (dependencies up) |
| `/version` | 8080 | build version |
| `/metrics` | 8080 | Prometheus/OTel metrics |

Quick liveness check while iterating: `curl -s localhost:8080/healthz`.

## The three env-var namespaces (don't mix them up)

- **CLI** (`crates/escurel-cli`): `ESCUREL_SERVER` (HTTP MCP URL, default
  `http://127.0.0.1:8080`), `ESCUREL_TOKEN`.
- **Your app's client** (your choice; the example uses):
  `ESCUREL_ENDPOINT`, `ESCUREL_TOKEN` (`examples/echo-app/src/lib.rs`).
- **The production server** (`docs/deploy/`): `ESCUREL_SERVER_DATA_DIR`,
  `ESCUREL_SERVER_LISTEN_HTTP`, `ESCUREL_CONFIG`,
  `ESCUREL_AUTH_*`, `ESCUREL_STORAGE_S3_*`. Your app doesn't set these —
  the deployment does.

## The iterate loop

1. Author/adjust seed pages (`references/07`) and your data model
   (`references/01`).
2. Write the failing test first (red), against the real gateway via
   `EscurelProcess` (`references/06`).
3. Implement the minimum to pass (green); rerun `cargo test`.
4. Poke ad-hoc with the CLI (`references/04`) when you want to *see* a
   tenant's state: `escurel list-skills`, `escurel expand <id>`,
   `escurel search "…"`.
5. Recovery when an index looks wrong: that's the operator-side `rebuild`
   tool (CLI-only ops surface), not an app concern — `references/10`.
