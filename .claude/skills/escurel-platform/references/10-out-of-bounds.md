# 10 — Out of bounds (hard prohibitions + cross-refs)

Read this before writing anything that feels like it belongs *inside*
Escurel. The boundary is the published surface (`references/02`–`05`);
everything below it is the service's job, and changes to it are **PRs
against this repo**, not workarounds in your app.

## Hard prohibitions

- **No Escurel internals in your app binary.** Don't depend on
  `escurel-server`, `Indexer`, `LaneStore`, `OidcVerifier`, the markdown
  parser, the embedder, or DuckDB. Your leaf dependency is
  `escurel-client` (Rust) or the wire/CLI surface (anything else). Pulling
  in `escurel-server` drags DuckDB + candle into your build — exactly what
  the leaf-crate guarantee (`references/05`) exists to prevent. (Tests are
  the one exception: `escurel-test-support` *does* embed the server, and
  that's fine — it's a `dev-dependency`.)

- **No raw SQL.** Reach relational/external data only through
  `run_stored_query`, which dispatches to a `[[query::*]]` page authored
  ahead of time with a declared `params:` schema. The dispatcher refuses
  SQL that isn't a query page. This is the sandbox; don't try to route
  around it.

- **No side-dooring the indexer.** Seed and write only through
  `update_page` (or `FixtureBuilder`, which uses it). Don't write markdown
  files into a tenant's data dir directly and don't poke DuckDB — the
  index would drift from canonical markdown.

- **No raw vector / embedding access.** Call `search`; the embedding model
  is an implementation detail. There is no `embed` tool.

- **No cross-tenant operations.** One server instance = one tenant.
  Federation is a separate, future layer; don't design your app assuming a
  single call can span tenants.

- **Don't reinvent auth.** Use `AuthMode::TestIssuer` + `mint_token` in
  tests (`references/08`); get a real bearer from the deployment's issuer
  in prod. Don't hand-roll JWKS/RSA in your app.

## Operator/admin surface — not an app concern

These exist but are **not** part of a normal consuming app:

- **CLI-only ops:** `audit` (drift detection), `rebuild` (re-index from
  markdown), `attach_external` (wire a DuckLake catalog),
  `export`/`import` (per-tenant backup). Operators run these.
- **Admin tools** (gated by `escurel:admin`): `admin_list_lanes`,
  `admin_lane_keys`, `admin_lane_blob`, `admin_index_query`. For operator
  UIs and tenant migration, not product features.

If your app thinks it needs one of these, that's a signal to step back —
either you're modelling something that belongs in the tenant's data
(`references/01`) or you're reaching for an operator capability that should
stay with the operator.

## When you genuinely need Escurel to change

A missing tool, a proto field, a new validation code, different indexer
behaviour — these are **PRs against this repo**, following `CLAUDE.md`
(red→green TDD, no-mock integration test, incremental ~400-LOC PRs). Adding
a tool is: `protocol.md` → `escurel.proto` → tonic regenerates → typed
method appears in `escurel-client`. Don't simulate the missing capability
with a hack in your app; raise it.

## Cross-references

- **`triton-platform`** — if your app chains through the Triton
  agent-ingress gateway, the end-to-end shape is
  `escurel → app-backend → triton → app-frontend` (`docs/spec/dx.md`
  §Chaining recipe). Triton has zero knowledge of Escurel; your backend is
  the integration point. `triton-tests::TritonProcess` composes with
  `EscurelProcess` in one harness.
- **`substrate-platform`** — deploying your app and Escurel on the DataZoo
  Hetzner substrate (Nomad/Consul/Vault/Fabio). Escurel itself is a pet
  stateful service (`docs/deploy/substrate.md`,
  `docs/deploy/escurel.nomad.hcl`); that skill covers the runtime contract
  for your app's own job.
