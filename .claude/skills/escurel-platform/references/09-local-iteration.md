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

`/metrics` worth knowing while iterating:
`escurel_writes_total{tenant,origin}` counts confirmed page writes by
origin `human` | `runner` — a write counts as `runner` when its
`update_page` `provenance` carries a `runner` or `workflow` key, else
`human`. Watch it to confirm your app's writes actually land, and stamp
provenance on your automated writers so the human/runner split stays
honest. Also there: `escurel_tool_calls`, `escurel_tool_latency_ms`,
`escurel_requests_total{route,status}`.

## The three env-var namespaces (don't mix them up)

- **SQL databases as rows** (`sql_view` + `instances: rows` over `sqlite` / `postgres`; `mysql` / `mariadb` are refused: `connector_not_supported`): the credential is a
  secret reference (per tenant: a file under `<ESCUREL_SECRET_FILE_DIRS>/<tenant>/`, or `ESCUREL_SECRET_<TENANT>__<NAME>`);
  a SQLite file and every `json_dir` / `parquet_dir` source must live under `ESCUREL_SQL_FILE_DIRS` (a gateway
  with it unset serves no file sources; `escurel-test-support` gateways expose the temp dir),
  a Postgres host must be public
  unless `ESCUREL_EGRESS_ALLOW_LOOPBACK=1` (local dev only). Tests that need Postgres run a real container
  (`--features live-postgres`).
- **Outbound calls to REST / MCP sources** (`openapi` / `mcp` skills; `ESCUREL_EGRESS_*`): the gateway
  refuses plain http and any loopback / private address by default. For a LOCAL outside system (a mock,
  a service on `127.0.0.1`) start the gateway with `ESCUREL_EGRESS_ALLOW_LOOPBACK=1`; never in
  production. Tunables: `ESCUREL_EGRESS_MAX_RESPONSE_BYTES` (4 MiB), `_TIMEOUT_MS` (10 000, max 30 000),
  `_MAX_CONCURRENCY` (8), `_RATE_PER_SEC` (50, per tenant+endpoint), `_WRITE_RETRY_BACKOFF_MS` (500).
  Secrets for an endpoint are referenced (`secret_ref`), e.g. `gsm:CRM_TOKEN` reads
  `ESCUREL_SECRET_<TENANT>__CRM_TOKEN` (tenant `acme` → `ESCUREL_SECRET_ACME__CRM_TOKEN`). In Rust tests, `escurel_test_support::ConfigOverrides.egress` takes an
  `EgressPolicy` (set `allow_loopback`).
- **The VS Code demo launcher** (`editors/vscode/demo/run.sh`, `ESCUREL_DEMO_*`): a self-contained
  gateway + runner + signed-in window for a consumer to look at the workbench. `ESCUREL_DEMO_HOME`
  (one demo per home), `ESCUREL_DEMO_CDP_PORT` (screenshots), `ESCUREL_DEMO_FOCUS=0` (classic IDE look),
  `ESCUREL_DEMO_S2D=0` (skip the Source-to-Deliver stories), `ESCUREL_DEMO_CODE_ARGS` (`--disable-gpu`).
  The full table is `editors/vscode/demo/README.md`. **Naming trap:** the Evolve agent's
  `ESCUREL_OIDC_ISSUER` / `_AUDIENCE` / `_JWKS_URI` are the AGENT's verifier settings, not gateway keys
  (those are `ESCUREL_AUTH_OIDC_*`); they become `ESCUREL_EVOLVE_OIDC_*` in 0.19.0 with the old names as a
  deprecated alias for one release.
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

## Make your test suite fast

`EscurelProcess::spawn` with `AuthMode::TestIssuer` mints a 2048-bit RSA
keypair for the in-process OIDC issuer. Your tests build with the `dev`
profile, where dependencies are unoptimized by default — and unoptimized,
that keygen measures a **mean of 4.88s** (median 3.01s, max 9.73s). One
spawn per test makes it the most expensive thing your suite does.

Put this in your workspace's root `Cargo.toml`:

```toml
[profile.dev.package.rsa]
opt-level = 3

[profile.dev.package.num-bigint-dig]
opt-level = 3
```

That is a ~21x cut on the keygen (4.88s -> 0.23s), and it costs you a
one-off compile of two pure-computation crates well outside your own code.
In escurel's own suite it took the whole workspace from 54 to 14 CPU-minutes.
A spawn then costs ~0.31s with the test issuer, ~0.10s without it.

`cargo nextest run` schedules across test binaries rather than running them
one at a time, which is worth having locally once you have more than a
couple. Measure before you put it in CI, though: it runs each test in its
own process, so a per-process setup cost (the test issuer's keypair, for
one) is paid per *test* rather than per binary. On escurel's own 2-core CI
runner that made it slower than plain `cargo test`, and faster only on a
many-core workstation.

### A2. A verifying gateway for a non-Rust harness: `escurel-test-gateway`

A TypeScript (or any non-Rust) integration suite cannot call `EscurelProcess`. When it needs a
gateway that **checks tokens** — to run a minted-mode runner, or to prove an editor against real
auth — start the binary the test-support crate ships:

```sh
cargo build --release -p escurel-test-support --bin escurel-test-gateway
escurel-test-gateway --tenant vsx --seed path/to/seed [--subject alice]
# stdout, ONE line, then it stays up until SIGTERM:
# {"gateway_url":"http://127.0.0.1:…","issuer_url":"http://127.0.0.1:…","kid":"…",
#  "signing_key":"-----BEGIN RSA PRIVATE KEY-----…","bearer":"eyJ…","admin_bearer":"eyJ…","tenant":"vsx"}
```

The seed holds `skills/*.md` and `instances/*.md` (a FLAT page id, `markdown/instances/<name>.md`) and, one level
down, `instances/<skill>/<id>.md`, which becomes the NESTED page id `markdown/instances/<skill>/<id>.md` — the
id of a row of an `instances: rows` skill and of its linked markdown.

It is the same in-process gateway and OIDC issuer the Rust suites use, so the claims cannot
drift from what the gateway expects. `--seed` is a directory of `skills/*.md` and
`instances/*.md`; each becomes `markdown/skills/<name>.md` / `markdown/instances/<name>.md`
(flat, so an instance is `<skill>__<id>.md`, as the shipped corpora lay them out). The `bearer`
is a human's (role `agent`, subject `--subject`): it can read, draft and promote. The
`admin_bearer` is the same subject with the admin role, for what only an admin may do (requeue,
pause and resume the runner). Both expire in ten minutes like every token this issuer mints.

A demo outlasts a ten-minute token. `--bearer-file <path>` writes `{"bearer", "admin_bearer"}`
there before the line is printed and replaces it (by rename; a reader never sees half a file) with
fresh ones every `--bearer-refresh-secs` (default 240), so whoever is signed in with it stays
signed in. Nothing else is written.

The gateway also holds a signing identity on the issuer's own key, so `mint_agent_token` works
against it (starting a skill in a terminal under a governed run needs that); a gateway without one
answers it `unsupported`.

Why a verifying gateway matters, and a verifier-less one cannot stand in for it: **only a token
can prove which run wrote something.** With no verifier the gateway has no claims at all, so a
runner's per-run token is ignored, an agent's draft carries no `run_id`, `list_lineage` shows
the event and the run but never the changeset, and promoting it never cascades. Give a
minted-mode runner `ESCUREL_RUNNER_AUTH_ISSUER=<issuer_url>`, `ESCUREL_RUNNER_AUTH_KID=<kid>`
and `ESCUREL_RUNNER_AUTH_SIGNING_KEY=<signing_key>` and leave `ESCUREL_RUNNER_TOKEN` unset.

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
