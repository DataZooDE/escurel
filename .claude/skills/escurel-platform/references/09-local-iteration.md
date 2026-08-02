# 09 — Local iteration: getting a gateway to develop against

## Three ways to get a gateway

`escurel-server` **is a binary** (`crates/escurel-server`, declared
`[[bin]]`), and running it directly is the ordinary local loop. It is
*also* still a library, consumed in-process by `escurel-test-support`; and
in production the same binary ships in the repo's container image
(`Dockerfile` → ghcr) that the substrate Kamal deploy launches
(`docs/deploy/substrate.md`).

Pick by what you are doing: **C** to have a gateway up and poke at it by
hand, **A** for the automated test loop, **B** to work against real data.

### A. In-process via `EscurelProcess` (the default, Rust)

For a Rust app, you almost never need a separately-running gateway: your
integration tests spawn one in-process (`references/06`). This is the
fastest, most hermetic loop and matches Escurel's own no-mock discipline.
Red→green:

```sh
cargo test -p <your-crate> <test_name>     # spawns escurel + your backend, asserts
```

To poke at a gateway *interactively*, prefer **C** below — it is a plain
binary now, so a throwaway `EscurelProcess::spawn` harness that parks until
Ctrl-C is no longer worth writing.

### B. Point at a deployed instance (any language)

For non-Rust apps, or to develop against real data, point your app/CLI at
a deployed `nonprod` gateway:

```sh
export ESCUREL_SERVER="http://<host>:8080"     # CLI (HTTP MCP)
export ESCUREL_TOKEN="<bearer from the real issuer>"   # references/08
# or for your app's own client: ESCUREL_ENDPOINT / your app's bearer
```

### C. Run `escurel-server` locally (any language)

The simplest way to have a real gateway on `:8080`:

```sh
cargo build -p escurel-server
ESCUREL_SERVER_DATA_DIR=/tmp/escurel-data \
ESCUREL_SERVER_LISTEN_HTTP=127.0.0.1:8080 \
ESCUREL_TENANT=default \
ESCUREL_EMBEDDING_PROVIDER=zero \
ESCUREL_SEED_DIR=examples/crm-demo \
  ./target/debug/escurel-server

# another shell
curl -s localhost:8080/healthz     # OK
escurel skill list                  # ESCUREL_SERVER defaults to :8080
```

Leaving `ESCUREL_AUTH_OIDC_ISSUER` unset runs the gateway **unauthenticated**
— no bearer needed, admin tools open. That is the point for local dev; see
`references/08` before exposing it anywhere.

Three traps worth knowing up front:

- **`ESCUREL_EMBEDDING_PROVIDER=zero` disables retrieval, silently.** Every
  vector is identical, so `search` ranking is meaningless and nothing is
  findable *by meaning* — pages still write, `list_instances` still works,
  and only search is dead, which is the last thing you look at. Fine for a
  keyless first boot; set `gemini` (+ `ESCUREL_GEMINI_API_KEY`) or
  `embeddinggemma` the moment you care about search. **Changing provider
  needs a re-embed** — one boot with `ESCUREL_REBUILD_INDEX_ON_BOOT=always`,
  or existing pages keep their old vectors.
- **`search` scores are reciprocal-rank fusion, not similarity.** The top
  hit is ~0.0164 (= 1/(60+1)) for every query. Rank carries the signal; the
  magnitude does not. Don't threshold on it.
- **Optional storage backends are cargo features.** `--features s3` / `gcs`
  / `duckvfs` (the DuckDB-VFS backend, e.g. a `gdrive://` corpus). A plain
  `cargo build`/`cargo test` overwrites the binary with one that lacks them,
  and you find out at boot: *"ESCUREL_STORAGE_BACKEND=… requires the `…`
  cargo feature; this binary was built without it."*

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
- **The server** (`docs/deploy/`, and option C above):
  `ESCUREL_SERVER_DATA_DIR`, `ESCUREL_SERVER_LISTEN_HTTP`, `ESCUREL_CONFIG`,
  `ESCUREL_TENANT`, `ESCUREL_SEED_DIR`, `ESCUREL_AUTH_*`,
  `ESCUREL_EMBEDDING_*`, `ESCUREL_STORAGE_*` (`_S3_*` / `_GCS_*` /
  `_DUCKVFS_*`), `ESCUREL_INDEX_BACKEND` + `ESCUREL_DUCKLAKE_*`. In
  production your app doesn't set these — the deployment does; locally you
  set them yourself. The authoritative list is the module doc comment at
  the top of `crates/escurel-server/src/config.rs`, which is generated from
  the same parser that reads them.

## The iterate loop

1. Author/adjust seed pages (`references/07`) and your data model
   (`references/01`).
2. Write the failing test first (red), against the real gateway via
   `EscurelProcess` (`references/06`).
3. Implement the minimum to pass (green); rerun `cargo test`.
4. Poke ad-hoc with the CLI (`references/04`) when you want to *see* a
   tenant's state: `escurel skill list`, `escurel page expand <id>`,
   `escurel search "…"`.
5. Recovery when an index looks wrong: that's the operator-side `rebuild`
   tool (CLI-only ops surface), not an app concern — `references/10`.
