# Changelog

All notable changes to escurel are recorded here. The format is
loosely [Keep a Changelog](https://keepachangelog.com/). Through the
`v1.0.x` line escurel used SemVer; from `v2026.07.13` onward versions
follow **`vYYYY.MM.DD`** — the date the binary set was cut (same-day
re-cuts append `.N`), matching the DataZoo release scheme (cf. erpl).

## Unreleased — BREAKING (stored format, wire, skills, agent behaviour)

**Read first:** [`docs/deploy/kind-migration.md`](docs/deploy/kind-migration.md) (stop-first upgrade, backup,
rollback) and the consumer checklist in
[`.claude/skills/escurel-platform/CHANGELOG.md`](.claude/skills/escurel-platform/CHANGELOG.md) (0.7.0 – 0.18.1).
Skill version `0.19.4`. Every consumer that writes pages or reads the tool surface moves in the same window.

### Fixed

- **List cursors are bound to their tenant and list; `ESCUREL_CURSOR_KEY` needs 32 bytes** (#684): a cursor issued
  for one skill/list no longer opens on another (it used to return a silently empty page); a shorter key stops
  the server at boot.
- **One `quote_ident` helper for every column spliced into rows SQL** (#685): a column named `we"ird` made
  `expand` a syntax error and `search` find nothing; a hostile name could close the identifier.
- **A run-bound token cannot file `escurel:` events, system events or workflow steps** (#686): the runner mints
  ADMIN run tokens, and the guard was `!is_admin`. The runner's own admin identity is unaffected.
- **A branch is its author's workspace** (#687): another machine can no longer write into, merge or abandon it;
  people and admins still decide it (the review).
- Links that escape the exposed directories fail closed when the directory cannot be read (#681); the
  `rows_get` timeout test is deterministic (#683).

- **Write-back: at-most-once unless the skill declares idempotency; `base_etag` is required (security / data
  integrity).** Every REST write used to count as repeatable, so an upstream that ignored `Idempotency-Key` could
  apply a change up to three times after a lost answer; an optional `base_etag` meant a proposal without one
  silently overwrote an upstream edit. A REST write is now retried only with `write: {…, idempotent: true}` (or
  `PUT`); a proposal without `base_etag` is refused `write_back_base_etag_required`.
- **Reading one row had no statement timeout (availability).** `expand` of a row page (`rows_get`) was the only
  rows read outside the watchdog, so a slow source held the tenant's single DuckDB connection (and every search
  and write behind it) as long as it liked. It now runs under the same `ESCUREL_ROWS_QUERY_TIMEOUT_SECS` interrupt
  as listing and search.
- **An unlistable directory failed the symlink check open (security).** `links_escape` (`json_dir` / `parquet_dir`
  sources, secret files) treated a directory it could not read as "nothing escapes"; it now fails closed.
- **A tenant could name another tenant's secret (security).** `ESCUREL_SECRET_<TENANT>__*` was matched by
  prefix over a lossy tenant encoding: tenant `a` matched `ESCUREL_SECRET_A__B__X` of tenant `a__b`, and
  `a-b` / `a_b` shared one namespace. The secret name may no longer contain `__`, an id whose token holds
  `__` has no env namespace, and `tenant_create` refuses a second tenant with the same token. Skill `0.19.2`.
- **A machine could land a skill page through a branch (security).** `merge_branch` judged each page of a
  machine's branch by an INSTANCE-shaped probe id, so a branch edit of an `autonomy: auto` skill's page, or a
  brand-new skill page, merged unreviewed (the skill page carries the gate's own configuration). The probe is
  now the page the merge would actually write; a machine's merge touching a skill page or a review skill is
  refused `review_required`, a person's merge is the review.

- **A refused read is an error, never an empty success (security-relevant).** `escurel-client` decoded
  the `structuredContent` of a refused read (`isError: true`, `{ok: false, issues}`) into a response type
  whose fields all default, so an ACL denial, `invalid_limit`, `query_not_found`, … returned `Ok(<empty>)`:
  a silent partial read. Now `Error::Refused(Refusal{issues, payload})` (shared reader
  `escurel_types::call_result`). The write family and `validate` still return the typed answer with
  `ok: false`; `rebase_pack` stays a report. Same fix in the echo and Gemini harness clients, the CLI
  (non-zero exit), the test-support client (`call_ok`), the VS Code client and the Dart client. A guard
  test fails if a crate reads a result's `structuredContent` outside the shared reader. Skill `0.15.1`.

### BREAKING

- **`ESCUREL_WRITE_ACL` defaults to `enforce`** (was `off`; an unrecognised value also enforces). Writes by a
  caller who is neither the instance's owner nor an admin are refused with `forbidden`. Set
  `ESCUREL_WRITE_ACL=off` to keep the old behaviour, or `log` to find the callers first. Skill `0.17.1`.

- **The page kind is `kind:` (was `type:`).** `type: skill|instance` is removed — a hard cut with no
  compatibility switch. A tenant whose lane still holds such pages boots **QUARANTINED** (up, answers only
  `migrate_kind` / `compact_lanes`); writes with the old key are refused (`frontmatter_type_removed`).
  Migrate with `escurel admin migrate-kind --tenant <t> [--apply]` (dry run by default, idempotent; rewrites
  pages, open drafts and CRDT snapshots; skips signed pack pages — the publisher re-exports). The `issue`
  skill's `kind` data field is now `issue_kind`.
- **Wire: `page_type` is `page_kind`** (`search` argument, `PageRef` answers, CLI `--page-kind`, Rust
  `PageKind`, Dart `PageKind`). A caller still sending `page_type` is refused, not silently unfiltered. The
  derived SQL column `pages.page_type` keeps its name.
- **Workflow-run pages: `status` → `run_status`** (migrated by `migrate_kind`; a tenant's own `status` data and
  the DB/API `status` fields are untouched).
- **A skill's `actions:` is a list of objects** (`{name, kind: event|prompt, label, event?, prompt?}`, Peacock's
  form); bare skill-id strings are rejected (`action_invalid`). `list_skills` returns the objects.
- **`resume_cursor` is removed — `next_cursor` is the only cursor name** (`list_inbox`, `list_events`, and the
  `list_*` tools). `next_cursor` is now where the page ENDED (present iff the page is non-empty; null only when
  there is nothing more); the new `has_more: true` says rows already lie past the page, so a client that pages
  "until `next_cursor` is absent" still terminates after one extra empty call. `list_drafts` / `list_changesets` /
  `list_branches` now take `limit` + `cursor` (the limit applies AFTER the caller's visibility filter).
- **Cursors are opaque** (`r1.` / `u1.` envelopes for rows and REST/MCP rows): a cursor from before the release
  answers `invalid_cursor` ("restart without `cursor`"). A `limit` outside the tool's declared range is refused
  with `invalid_limit`.
- **MCP `tools/call`: `content[0].text` is a short summary**; `structuredContent` is the full payload. A client
  that parsed the text block as JSON must read `structuredContent`; a client that can read ONLY the text block
  (some chat hosts) now sees the summary, not the data.
- **Read tools answer domain mistakes as `isError: true` + `issues[]`** (the shape write tools always had):
  `invalid_cursor`, `field_not_filterable`, `query_not_found`, `query_not_runnable`, `invalid_query_params`,
  `endpoint_not_registered`, `use_write_back`. JSON-RPC errors remain for malformed requests.
- **`autonomy: review | confirm` is ENFORCED at the gateway for MACHINE callers** (tokens carrying `run_id` /
  `skill` / `act.sub` claims): `update_page` and the `close_session` write-through answer
  `{ok: true, held_for_review: true, draft}` and nothing lands until a reviewer promotes; `move_page` /
  `delete_page` answer `review_required`. People on plain agent-role tokens, admins and `autonomy: auto` skills
  are unchanged, and promoting always lands. An agent flow that wrote review-skill pages directly must now propose
  drafts.
- **Write-back drafts can only be promoted by a non-agent token** (`promote_requires_human`); a run token can
  propose a write-back but never approve it.
- **Credentials a tenant may reference are allow-listed** (`secret_ref`): `gsm:` / `env:ESCUREL_SECRET_*`, extra
  `env:` names only via `ESCUREL_SECRET_ENV_ALLOW`, `file:` only under `ESCUREL_SECRET_FILE_DIRS`
  (default `/run/secrets`). An existing endpoint registered with an arbitrary `env:`/`file:` reference stops
  resolving until the operator allows it.
- **`trust` on projections is `external` (REST/MCP) or `source` (SQL rows)**; treat both as data, never as
  instructions.
- Details line by line: [`docs/notes/breaking-wire-changes.md`](docs/notes/breaking-wire-changes.md).

### Added

- **Migration tooling (#654).** `escurel admin migrate-kind` runs the apply in a spawned server task (a client
  that disconnects or is killed no longer cancels it; `--timeout-secs` is opt-in, progress is printed every
  30 s); a durable marker `meta/migrate-kind.pending` keeps a tenant QUARANTINED across a `kill -9` mid-apply
  and the re-apply is idempotent; `escurel admin migrate-kind-files --apply <dir>` rewrites a checkout
  offline (temp files are created `O_EXCL`, a planted symlink is never followed); BOM / CRLF pages parse and
  migrate; `scripts/migrate-kind-job.sh` is the stop-first one-shot job (non-zero on a conflict or a boot
  failure). Measured: 20k pages ≈ 11 min, 406 MB peak.
- **Operator safety (#654).** `config_keys` registers every `ESCUREL_*` variable and generates
  `docs/deploy/env.md` (a test fails on an undocumented or phantom key); the structured log goes through a
  bounded non-blocking writer (a stalled reader can no longer freeze the gateway; dropped lines are counted
  in `escurel_log_lines_dropped_total`, lines are capped at 16 KB); a non-loopback listener with no OIDC
  issuer logs a loud warning and a `/readyz` notice; Postgres attaches carry `connect_timeout`
  (`ESCUREL_SQL_CONNECT_TIMEOUT_SECS`) and the rows query bound is `ESCUREL_ROWS_QUERY_TIMEOUT_SECS`;
  orphan `*.md.tmp` files are swept at boot; CI runs the live-postgres suites nightly, the full ten-point
  SIGKILL test, a PR-time Docker image build, and tests that used to skip silently are `#[ignore]`d.
- **Typed-client refusals (#654).** `escurel-client`, the VS Code client and the Dart client treat
  `isError: true` as an error (`Error::Refused`), never as an empty result; `held_for_review` and `draft`
  are typed on `update_page`.
- **Anofox Evolve (#653, #657, #658).** Owner-scoped Evolve controls: `evolve_validate` and the new
  `evolve_compare` control labels target an owner-private `evolve_comparison` page, are stamped and attested
  by the gateway, never dispatched by the runner; prepared training sources and private holdout terms are
  reviewed from the Workbench. The VS Code extension gains a **Scenarios** view (seed-vs-winner diffs over the
  signed-in token, a page is trusted only if it carries the record's result hash), "New scenario comparison"
  and the Compute comparison action, plus Evolve plan review in the native window.
- **DuckDB 1.5.6 (#659).** `duckdb` / `libduckdb-sys 1.10506.0`; the image's `DUCKDB_VERSION=v1.5.6`.
  DuckDB extensions are version-locked, so `anofox_optimize` and `gdrive` were rebuilt for 1.5.6 (the erpl
  mirror now serves both). The `gdrive` bake is conditional: when the mirror has no artifact for the pinned
  DuckDB the image is built WITHOUT gdrive with a loud warning (`REQUIRE_GDRIVE=1` fails the build instead);
  the `anofox_inventory` extension test skips with a clear message on a DuckDB-version mismatch.
- **VS Code workbench (#660).** **Focus mode** (`Switch to focus view` / `Leave focus view`: a calm window
  without menu bar, command center, status bar, breadcrumbs or minimap; the person's own settings are
  remembered and restored exactly) and the **Overview board** ("Today": decisions waiting, agent activity,
  needs attention, open items per skill, recently finished) as the first screen; the light **Escurel Calm**
  theme with WCAG contrast guards (thread connectors have arrowheads and ≥ 3:1 contrast, outlined state chips,
  button borders); editor tabs name the page or skill; the run trace has a time axis and each step opens to
  what it asked and got back; records whose skill names a report show that report's KPI figures and tables;
  "Preview with parameters" on query pages; "Explain this view" on every view; keybindings `ctrl+alt+r/a/i/k`.
- **Source-to-Deliver demo (#660).** `editors/vscode/demo/s2d/`: the three S2D stories (supplier exception,
  transport consolidation, aftermarket last-time-buy) synced from the shared seed, rehearsal script, held
  proposals in "Awaiting you", optimizer pages when the `anofox_optimize` extension matches the gateway's
  DuckDB. Anonymised, illustrative data.
- **Web workbench (#660).** `deploy/web-workbench/`: a code-server image with the VSIX preinstalled, the
  calm layout baked in, an Escurel-skinned login page generated from the Calm theme tokens, a password gate
  (12 characters by default, `MIN_PASSWORD_LENGTH` may lower it to 8, placeholders refused), no terminal
  (`node-pty` removed), no marketplace, non-root, read-only root filesystem; `compose.yaml` +
  `compose.s2d.yaml` give the full S2D demo in one command, `compose.proxy.yaml` a Caddy TLS front;
  `s2d-up.sh` / `s2d-reset.sh`; `scripts/web-workbench-smoke.sh`. See `docs/deploy/web-workbench.md`.
- `folder:`, `role:`, `tags:` and the OKF vocabulary (`title`, `resource`, `generated`, `verified`, `status`,
  `stale_after`, `sources`) on skill pages; the Knowledge tree in the VS Code extension shows them.
- `backend.instances: rows` (one instance per row of a `sql_view`, optional linked markdown), REST (`openapi`)
  and MCP rows, `describe_backend`, and **human-gated write-back** (draft + promote, etag conflict check,
  idempotency key, bounded retries, dead-letter, audit).
- **SQL rows over real databases and write-back to them.** `connector: postgres | mysql | sqlite` with
  `instances: rows`; credentials as `secret_ref` references (inline `secret` deprecated), checked against the
  egress policy (`ESCUREL_SQL_FILE_DIRS` for SQLite files); a row of a skill with `writable_columns` changes
  through the same draft → human promote → guarded single-transaction `UPDATE` flow as REST/MCP rows. Postgres
  attaches enforce the statement timeout server-side. The image bakes the `sqlite` DuckDB extension next to
  `postgres`. Verified against a real Postgres container and a real SQLite file. Conflict detection is
  etag-only (the row's values at read time); a `version_column:` option was considered and dropped (2026-10-09).
  **MySQL / MariaDB are refused (skill `0.19.3`)**: it has no integration test, no CI job, no timeouts and was never
  DNS-pinned like Postgres, so `register_credential` answers `connector_not_supported`, a skill page with
  `backend.source.connector: mysql` fails `validate` and is not stored, and the image does not bake
  `mysql_scanner`.
- **Run traces keep what each tool call asked and got back.** `get_run_tool_calls` rows gain optional
  `args_summary` / `result_summary` (2 KB, credentials redacted by key and by pattern, content bodies reduced
  to sizes); the VS Code run detail shows them per step. `ESCUREL_TOOLCALL_DETAIL=off` records sizes only.

### Operators

- **`/readyz` reports quarantine** without failing: still 200 (so the migration can run) with
  `x-escurel-quarantined: 1` and a JSON `notices` body; new metrics `escurel_tenant_quarantined{tenant}`,
  `escurel_migration_pending`, `escurel_semantic_search_enabled`, `escurel_egress_total{outcome}`,
  `escurel_write_back_total{outcome}`, `escurel_source_unavailable_total{kind}`. **Do not gate traffic on the
  status code alone**; deploy stop-first with the migration before the swap.
- **New env (every key is in the generated table [`docs/deploy/env.md`](docs/deploy/env.md), the source of truth; `docs/deploy/README.md` explains the groups):** `ESCUREL_EGRESS_ALLOW_LOOPBACK` (dev/tests only —
  never in production), `ESCUREL_EGRESS_MAX_RESPONSE_BYTES`, `…_TIMEOUT_MS`, `…_MAX_CONCURRENCY`,
  `…_RATE_PER_SEC`, `…_WRITE_RETRY_BACKOFF_MS`, `ESCUREL_SECRET_<NAME>`, `ESCUREL_SECRET_ENV_ALLOW`,
  `ESCUREL_SECRET_FILE_DIRS`, `ESCUREL_SHUTDOWN_DRAIN_SECS` (graceful-stop deadline, default 25). An unparsable `ESCUREL_EGRESS_*` value now **fails the boot** (it used to be
  silently ignored).
- The server image runs **non-root (uid 65532)**, ships the `escurel` CLI (`docker exec … escurel admin …`), and
  fetches the gdrive DuckDB extension over https. `escurel-server --help` / `--version` no longer boot the
  server.
- `tenant export` is refused while a tenant is quarantined: take the pre-upgrade backup with the server stopped
  (`tar` the tenant directory), see the runbook.

## v2026.07.13

### Changed

- **BREAKING: removed the gRPC transport.** HTTP (MCP-over-HTTP +
  WebSocket) is now the only transport. Deleted the `escurel-proto`
  crate and the `:8081` gRPC listener. `escurel-client` now speaks
  MCP-over-HTTP, and the `escurel` CLI / `escurel-tui` default
  `--server` to `http://127.0.0.1:8080`. Admin/operator capabilities
  are now admin-role-gated MCP tools on `POST /mcp` rather than a
  separate gRPC service. Long-running admin ops (`rebuild`,
  `compact_lanes`, `tenant_export`, `tenant_import`) are blocking
  JSON-RPC calls that return their final result directly; tarballs are
  carried base64-encoded in the JSON (`tenant_export` →
  `{tarball_b64, bytes}`, `tenant_import` takes
  `{tenant_id, tarball_b64}`) rather than as gRPC streams.
  `live_session` runs over the WebSocket at `/ws`.

### Client

- `escurel-client` admin + streaming surface: an `AdminClient` for the
  unary `EscurelAdmin` RPCs (tenant CRUD, audit, quota, health,
  `attach_external`, `embedding_reload`, `compact_lanes`) plus the
  server-streaming (export / rebuild / compact) and client-streaming
  (import) flows, and the agent event/validate RPCs (`capture_event` /
  `list_inbox` / `list_events` / `assign_event`, `validate`).

### CLI

- Rebuilt the `escurel` CLI as a gh/aws-style noun-verb tree over
  `escurel-client` (`skill`, `instance`, `page`, `link`, `event`,
  `query`, `chat`, `admin`, plus top-level `search` / `resolve`), with
  a global `--format json|table` flag and a JSON-on-stderr error
  contract (non-zero exit) for agent consumption.
- New `escurel ui` subcommand launches the interactive terminal
  browser against the same `--server` / `--token`.

### TUI

- New `escurel-tui` crate: a k9s-style interactive terminal UI
  (ratatui + crossterm) over `escurel-client`. Elm-style `App`
  (navigation stack skills → instances → entity, inbox + per-instance
  event history, outgoing links + backlinks, `/` filter, `?` help)
  with a panic-safe terminal guard and a real crossterm event loop.
  Logic is terminal-free and exercised against a real gateway via a
  ratatui `TestBackend` (no mocks); run it with
  `scripts/verify-tui.sh`.

## [1.0.0] — 2026-05-26

First stable release. The v1 cut-line in
[`docs/spec/roadmap.md`](docs/spec/roadmap.md) is met.

### Agent surface (14 tools, on MCP-over-HTTP)

- Read: `search` (hybrid vector + FTS, RRF-fused), `resolve`,
  `expand`, `neighbours`, `list_skills`, `list_instances`,
  `run_stored_query`, `validate`.
- Write: `update_page`.
- Live CRDT: `open_session` / `apply_op` / `close_session` over
  HTTP and the WebSocket `/ws`
  attach path (Loro engine, per-page `LiveDoc` actor, op-log +
  snapshot persistence, two-stage external-edit reconciler).
- Chat history: `append_message` / `list_messages` (per-chat-group
  conversation log).

### Admin surface (admin-role-gated MCP tools)

Tenant CRUD, streaming export/import, audit, streaming rebuild,
`attach_external` (read-only external catalog), `embedding_reload`
(degraded-start recovery), `compact_lanes` (subsumed op compaction),
`quota_get`, and an auth-free `health`.

### Storage & retrieval

- DuckDB-only per-tenant store (vss + fts extensions); HNSW dense
  vectors + FTS, fused with Reciprocal Rank Fusion.
- LaneStore trait with **S3 (Hetzner Object Storage) as the
  production default** and a local-FS dev backend.
- Crash-recovery: mid-write transaction rollback; cattle-node-loss →
  automatic rebuild-from-markdown on boot.

### Embeddings

EmbeddingGemma in candle (CPU default) behind a reloadable seam;
Gemini as an optional hosted provider; a zero/hash embedder for the
dev loop.

### Transports, auth, quotas

axum HTTP gateway (MCP/JSON-RPC framing + `/ws`),
OIDC JWT verification with JWKS caching, token-bucket quotas across
three dimensions (queries, writes+embeds, concurrent sessions).

### Operability

- 12-factor `escurel-server` binary: `ESCUREL_*` config (over TOML),
  ports 8080/8081, graceful SIGTERM, degraded-start.
- OpenTelemetry traces + Prometheus `/metrics` + structured JSON
  logs with `request_id`.
- Substrate deployment artefacts: Nomad jobspec set, Packer
  golden-image fragment, tenant-export shipper periodic job,
  Tailscale ACL fragment, and a three-target deploy guide.
- `cargo deny` license + advisory audit; `Cargo.lock` committed.

### Developer experience

- `escurel-client` typed RPC wrapper (leaf crate — no server deps).
- `escurel-test-support`: `EscurelProcess` + `AuthMode::TestIssuer`
  + `FixtureBuilder` + `McpTestClient` — spawn escurel in a
  downstream app's tests without re-deriving the JWKS/RSA harness.
- `escurel` CLI; `examples/echo-app` demonstrating the chaining
  recipe.

### Engineering process

Built red→green TDD with no-mock integration tests as the merge
gate. GitHub Actions CI (paused during bootstrap) is **re-enabled**
at this release for every push to main and every PR.
