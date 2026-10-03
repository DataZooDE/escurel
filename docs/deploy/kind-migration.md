# Runbook: migrating a tenant from `type:` to `kind:` (escurel skill 0.8.0)

The page-kind frontmatter key `type: skill|instance` was **removed**; it is `kind:` now. A tenant that still
holds pages with the old key boots **QUARANTINED**: the server is up, but every MCP tool except
`migrate_kind` and `compact_lanes` answers `tenant_quarantined`, and `rebuild` refuses. The migration runs
against that running, quarantined tenant.

## 0. Before

Deploy the new engine first: each un-migrated tenant comes up quarantined. Then:

- Stop (or quiet) writers: the lane store has no compare-and-swap; the tool re-reads each page before
  writing and reports one that moved as a conflict.
- Close live editing sessions (the tool refuses `--apply` while any page has CRDT ops newer than its newest
  snapshot): close them, or run `escurel admin compact-lanes --tenant <t>`.
- Take a `tenant export` if you want a restore point.

## 1. Dry run (the default)

```sh
escurel admin migrate-kind --tenant acme
```

Read the report: `pages_to_migrate`, `drafts` (open drafts that will be rewritten; their `content_sha256`
changes), `snapshots_to_rewrite`, `run_status_renamed` (workflow boards), and the things that need a human:

- `conflicts`: a page with BOTH a legacy `type:` and a `kind:` (usually its own `kind:` data field). Rename the
  data field in that page, then rerun. The tool never auto-fixes these.
- `skipped_pack_base`: signed pack pages (`markdown/base/**`). They cannot be rewritten locally. The pack
  publisher must re-export and re-sign the pack with `kind:`; then `escurel admin pack rebase`.
- `crdt_pages_with_live_ops`: close the session / compact the lanes.

## 2. Apply

```sh
escurel admin migrate-kind --tenant acme --apply
```

Idempotent: a second run reports nothing to migrate. It records an `escurel:kind-migration` system event
(the audit trail for the rewritten drafts) and returns its id.

## 3. What "done" looks like

Boot scans the lane (every boot). `--apply` rewrites the lane, REBUILDS the index with the real embedder and
lifts the quarantine: the response says `tenant_quarantined: false`. If it says `true`, something legacy is
left (a conflict, or a signed pack page): fix it (or have the publisher re-export) and rerun. There is no
environment switch that accepts the old key.

The gate covers MCP `tools/call`. `/ws` and `/ingest` are not gated; the `/readyz` probe does not yet report
quarantine, so watch the boot log line `tenant QUARANTINED` (and the `tenant_quarantined` answers) instead.

## 4. Consumers

Every writer must emit `kind:` (templates, scaffolds, agent prompts and skills) and send `page_kind` to
`search`. See the consumer checklist in `.claude/skills/escurel-platform/CHANGELOG.md` (0.8.0).

## Rollback

The migration only changes pages, open drafts and historical snapshots, in place. To go back: restore the
`tenant export` taken in step 0 and deploy the previous engine. Rewriting `kind:` back to `type:` is not
supported.
