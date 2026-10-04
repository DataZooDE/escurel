# Gotchas from the `type:` -> `kind:` hard cut (OKF stage 1)

Written while doing the rename, so the next one (there will be one) does not rediscover them.

## 1. A page may have its OWN data field named `kind`

**Symptom.** After the parser learned `kind:`, the compile-first `issue` pages stopped parsing and three
`escurel-runner` end-to-end tests failed ("no issue of kind lint_summary within 45s"). Those pages carry
`type: instance` AND their own `kind: lint_summary` (the issue's category).

**Fix.** The parser takes the first of `kind` / `type` whose VALUE is `skill` or `instance`, not the first key
present; the engine-owned `issue` skill's data field was renamed `issue_kind`; `migrate_kind` reports a page
with both a legacy `type:` and a `kind:` as a conflict instead of producing two `kind:` keys.

**Recognise it.** A new top-level key that is also the name of a data field somewhere. Grep
`required_frontmatter:` and the corpus strings for the name before adopting it.

## 2. Tool arguments that ignore unknown keys turn a rename into a silent filter drop

**Symptom.** `search` with the renamed `page_kind` returned every page: the server's argument struct still
had `page_type` and silently ignored the unknown key. Only the client contract-parity test noticed.

**Fix.** Rename the argument everywhere, and refuse the REMOVED name loudly (`search: page_type was renamed
page_kind`) so an old client cannot silently lose a filter.

## 3. Do not rename identifiers with a quote-aware sed; let the compiler tell you

**Symptom.** A script that skipped `page_type` inside string literals still rewrote multi-line SQL
continuation lines (`WHERE page_type = 'skill' \`), because a continuation line starts outside a quote.

**Fix.** Rename the struct field definitions by hand, then apply rustc's own error spans
(`cargo check --message-format=json`, primary span of "no field `page_type`") in a loop. SQL strings are never
touched because the compiler never reports them. The SQL column `pages.page_type` is a derived-index column
and keeps its name.

## 4. A lineage node's `type` is not the page kind

`type: run|event|changeset|instance` on lineage nodes (and in the VS Code lineage fixtures) is a different
namespace from the page-kind frontmatter key. A repo-wide sed of `"type": "instance"` would have broken them.
Only page-frontmatter dumps (expand / list_instances / search fixtures) were flipped.

## 5. DuckDB: a second `Connection::open` on the same file is a different instance

**Symptom.** A test inserted into `crdt_ops` through a fresh `Connection::open(path)` and the indexer (a
different connection) did not see the row. Use `conn.try_clone()` on the indexer's own database instead.

## 6. The lane store has no compare-and-swap

`LaneStore::write` returns a version but accepts no expected version. `migrate_kind` re-reads each page right
before writing and reports a page that moved as a conflict; it cannot make that atomic. Run it with writers
quiet (it also refuses `--apply` while a CRDT session is live).

## 7. `rebuild` aborts on the first unparseable page, AFTER truncating

Before the cut a legacy page would have taken a whole tenant down one page at a time (the loop used `?` after
the `DELETE`s). `rebuild` now scans every lane page FIRST, collects all legacy pages and refuses once, before
anything is truncated; boot does the same scan for a surviving index.

## 8. A refuse-to-boot gate makes its own migration unrunnable

**Symptom.** The first design refused to boot a tenant with legacy pages and shipped the migration as an
admin tool of the NEW engine, so the tool could never run on the tenants that need it.

**Fix.** Quarantine instead of failing: the tenant boots, serves only `migrate_kind` / `compact_lanes`, and
`migrate_kind --apply` rewrites the lane, rebuilds the index and lifts it. Any "refuse" gate on a state that
only an in-engine tool can repair must leave that tool reachable.

## 9. The pre-push hook flakes under load

`escurel-runner`'s suite (`run_control_subscriber`, workflow tests) fails intermittently when the hook runs
alongside other builds; it passes alone. Rerun the push.
