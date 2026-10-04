# Rolling a breaking release across consumer repos (`type:` -> `kind:`, skill 0.8.0 and later)

Operator view. The engine procedure itself (backup, quarantine, `migrate-kind`, rollback) is
[`kind-migration.md`](kind-migration.md); this page is about the **repositories and services that depend on
the engine** and the order to change them in. The measured state of every repo on the maintainers' machine
(counts, exact paths) is the survey [`docs/notes/2026-10-04-consumer-migration-survey.md`](../notes/2026-10-04-consumer-migration-survey.md).

## The one fact that sets the order

A consumer is tied to the engine only **when it is deployed against it** (or when it bumps its vendored
`escurel` pin). A consumer that vendors escurel (`vendor/escurel` submodule) builds and tests the PINNED engine,
so it keeps working the day the new engine is released. The cut bites in two places only:

| situation | what the consumer sees |
|---|---|
| new engine, tenant not yet migrated | the tenant is **quarantined**: `/readyz` 200 with `x-escurel-quarantined: 1`, every MCP call answers `tenant_quarantined` (JSON-RPC error, `retryable: false`, names the offending pages and the migration command); `/ingest`, `/blob/*`, `/ws` answer **503 `tenant_quarantined`** |
| new engine, a client still writes `type: instance` | the write is refused with `frontmatter_type_removed` (`location: frontmatter.type`, a `suggestion` naming the fix) |
| new engine, a client still sends `page_type` to `search` | refused: "renamed `page_kind`" |
| old engine, a client already writes `kind:` | refused by the old parser: never deploy updated writers against an old engine |

So: **prepare every consumer on a branch first** (nothing there needs the new engine), then flip engine and
consumers together, **stop-first**, one environment at a time.

## Prepare (before the window; no production risk)

1. For each repo that holds page files: `escurel admin migrate-kind-files --path .` (a dry run; add `--apply`
   on a clean git tree and commit the one diff). It leaves user data fields named `type:` alone and **refuses
   a page that also has a `kind:` key**: that is a data field named `kind` (`kind: code`, `kind: saas-api`) and
   must be renamed first (for example `skill_kind`), exactly as the engine's own `issue` skill became
   `issue_kind`. It never enters submodules or nested repos (bump their pin instead) and reports signed pack
   `base/` pages (the publisher re-exports those).
2. Change what WRITES or TEACHES the old key: `git grep -E "type:[[:space:]]*(skill|instance)"` over src, scripts,
   prompts, templates, test fixtures; `page_type` -> `page_kind`; `resume_cursor` -> `next_cursor`. The gate: that
   grep is empty outside vendored code.
3. Bump pins in dependency order (a template before the products that vendor it), run each repo's own
   real-stack tests against the new pin.
4. Re-export and re-sign any signed skill pack whose `base/` pages carry the old key.

Transcript (real binary, throwaway git repo with one conflicting page):

```text
$ escurel admin migrate-kind-files --path .          # dry run
  summary: files_scanned 3, to_migrate 2, conflicts 1
  migrate:   instances/customer/acme.md  @@ line 2 @@  -type: instance  +kind: instance
             skills/customer.md          @@ line 2 @@  -type: skill     +kind: skill
  conflicts: skills/tabelle.md  both `type:` and `kind: code` are present. If `kind:` is your own data field, rename it first ...
$ escurel admin migrate-kind-files --path . --apply  # 2 pages rewritten, 1 conflict left untouched
$ escurel admin migrate-kind-files --path .          # again: to_migrate 0, already_kind 2   (idempotent)
```

## The window (per environment, stop-first)

1. Stop the consumers, then the engine. Back up each tenant data dir with the server stopped
   (`tar` of `/data/tenants/<t>`): this is the restore point (`tenant export` is refused once quarantined).
2. Start the NEW engine image. Un-migrated tenants boot quarantined; alert on
   `escurel_tenant_quarantined == 1`.
3. `escurel admin migrate-kind --tenant <t>` (dry run: pages, drafts, snapshots; conflicts must be empty), then
   `--apply`. It refuses while a CRDT editing session is live: schedule the window with no editor open.
4. Start the NEW consumer versions; smoke one write and one read per consumer. Check
   `escurel_tenant_quarantined` is `0` and `escurel_migration_pending` is `0`.
5. **Rollback:** stop everything, restore the tar, start the OLD engine image and the OLD consumers.

## After

Keep `escurel admin migrate-kind-files --path .` as a CI check in repos that hold page files: a clean dry run
means no legacy page has come back (a stale branch merged, a template copied from an old repo).
