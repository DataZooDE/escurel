# Changelog

All notable changes to escurel are recorded here. The format is
loosely [Keep a Changelog](https://keepachangelog.com/). Through the
`v1.0.x` line escurel used SemVer; from `v2026.07.13` onward versions
follow **`vYYYY.MM.DD`** — the date the binary set was cut (same-day
re-cuts append `.N`), matching the DataZoo release scheme (cf. erpl).

## Unreleased — BREAKING (stored format, wire, skills, agent behaviour)

**Read first:** [`docs/deploy/kind-migration.md`](docs/deploy/kind-migration.md) (stop-first upgrade, backup,
rollback) and the consumer checklist in
[`.claude/skills/escurel-platform/CHANGELOG.md`](.claude/skills/escurel-platform/CHANGELOG.md) (0.7.0 – 0.11.0).
Skill version `0.11.0`. Every consumer that writes pages or reads the tool surface moves in the same window.

### BREAKING

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
- **`resume_cursor` is removed** — `next_cursor` is the only cursor name.
- **MCP `content[0].text` is now a short summary**; `structuredContent` is the full payload (clients that parsed
  the text block as JSON must read `structuredContent`).
- **`autonomy: review|confirm` is enforced at the gateway**: an agent-role `update_page` on such a skill becomes
  a draft (`held_for_review`) instead of landing directly.
- **Write-back drafts can only be promoted by a non-agent token** (`promote_requires_human`).
- More breaking wire changes, one line each, are appended to
  [`docs/notes/breaking-wire-changes.md`](docs/notes/breaking-wire-changes.md) as they land.

### Added

- `folder:`, `role:`, `tags:` and the OKF vocabulary (`title`, `resource`, `generated`, `verified`, `status`,
  `stale_after`, `sources`) on skill pages; the Knowledge tree in the VS Code extension shows them.
- `backend.instances: rows` (one instance per row of a `sql_view`, optional linked markdown), REST (`openapi`)
  and MCP rows, `describe_backend`, and **human-gated write-back** (draft + promote, etag conflict check,
  idempotency key, bounded retries, dead-letter, audit).

### Operators

- **`/readyz` reports quarantine** without failing: still 200 (so the migration can run) with
  `x-escurel-quarantined: 1` and a JSON `notices` body; new metrics `escurel_tenant_quarantined{tenant}`,
  `escurel_migration_pending`, `escurel_semantic_search_enabled`, `escurel_egress_total{outcome}`,
  `escurel_write_back_total{outcome}`, `escurel_source_unavailable_total{kind}`. **Do not gate traffic on the
  status code alone**; deploy stop-first with the migration before the swap.
- **New env (all documented in `docs/deploy/README.md`):** `ESCUREL_EGRESS_ALLOW_LOOPBACK` (dev/tests only —
  never in production), `ESCUREL_EGRESS_MAX_RESPONSE_BYTES`, `…_TIMEOUT_MS`, `…_MAX_CONCURRENCY`,
  `…_RATE_PER_SEC`, `…_WRITE_RETRY_BACKOFF_MS`, `ESCUREL_SECRET_<NAME>`, `ESCUREL_SECRET_ENV_ALLOW`,
  `ESCUREL_SECRET_FILE_DIRS`. An unparsable `ESCUREL_EGRESS_*` value now **fails the boot** (it used to be
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
