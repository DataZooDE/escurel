# Fixture seeding bypasses the quota gate and mirrors into the LaneStore

**Symptom (1).** During PR M-DX-3 (the test-boilerplate retirement),
the `escurel-server` `auth_quota::write_tool_debits_writes_dimension_independently`
test failed when ported to `escurel-test-support`. The test installs
a `writes_per_minute = 1` quota and expects the *first* test write
to succeed and the second to 429. With fixtures seeded through the
gateway's MCP `update_page` tool, the per-tenant fixture seed had
already burned the only write token before the test body ran.

**Symptom (2).** The `grpc_admin_crud::audit_returns_clean_drift_for_seeded_tenant`
test failed because `Indexer::audit()` compares the on-disk
markdown (`LaneStore::list("markdown/")`) against indexed page rows
(`SELECT page_id FROM pages`). Seeding via `Indexer::update_page`
alone updates DuckDB but not the LaneStore, so every fixture page
shows up under `indexed_but_no_markdown` and the audit assert
trips.

**Fix.** In `crates/escurel-test-support/src/process.rs::seed`:

1. Use `Indexer::update_page` directly (not the gateway's MCP tool)
   so the gateway's auth + quota middleware doesn't sit in front of
   the fixture seed. The contract in
   [`docs/spec/dx.md`](../spec/dx.md) §"Fixture/seeding façade"
   commits to "what tests seed is what `update_page` would seed
   in production" — the gateway's tool body is literally
   `indexer.update_page(...)`, so calling that directly is the
   same write path with the middleware lifted off.
2. Mirror each fixture body into the default LaneStore via
   `FsStore::write` *before* invoking `indexer.update_page`. This
   plugs the hole that the gateway's `update_page` tool itself
   doesn't write canonical markdown to the LaneStore — a known
   gap that's outside this PR's scope, but one we have to paper
   over so audit-style assertions in the admin-CRUD tests stay
   clean.

**Recognition.** If a test that pre-seeds via `FixtureBuilder`
unexpectedly trips quota / 429s, or `Indexer::audit()` reports
drift that wasn't there in the prior in-test seed-via-Indexer
harness, suspect either the middleware-in-front-of-seed or the
LaneStore-not-mirrored path. Both are addressed by the support
crate's `seed` today; future regressions live in
`crates/escurel-test-support/src/process.rs`.

**Forward-looking.** If the gateway's `update_page` tool ever
starts writing canonical markdown to the LaneStore (it should —
the LaneStore is the source of truth for `rebuild`), the
support-crate mirror becomes a no-op rather than a contract
violation. Track in the spec rather than in this note.
