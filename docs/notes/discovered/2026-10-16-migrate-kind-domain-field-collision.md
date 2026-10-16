# A tenant's own `kind` data field blocks `migrate-kind`: rename it inside the migration

## Symptom

The heron-escurel lab rehearsal (2026-10-16, Drive-backed tenant `datazoo-loops`) stopped at the dry run:
`the dry run reports conflicts (a page with both keys)` for `markdown/instances/system/escurel-event-bus.md` and
`lexware.md`. Both are `type: instance` AND carry a domain field `kind: bus` / `kind: saas-api`; the `system`
skill declares it in `optional_frontmatter: [kind, endpoint, ...]`. After the hard cut `kind:` IS the page kind, so
renaming `type:` -> `kind:` would have produced two `kind:` keys (or made `kind: bus` the page kind). The
engine refused safely; the only way forward was to hand-edit live pages first.

## Fix

`escurel admin migrate-kind --rename-field OLD=NEW` (repeatable; MCP `migrate_kind.rename_fields`; job env
`ESCUREL_MIGRATE_KIND_RENAME=kind=system_kind`). Inside the migration it renames the colliding top-level field on
every page that still has the legacy `type:`, and the entry in the `optional_frontmatter`/`required_frontmatter`
lists of the skills that declare it (text edits: values, comments, CRLF and BOM kept), then `type:` -> `kind:`.
A target that already exists is a collision (reported, never overwritten). Other mentions of the old name in a
skill (SQL, listing columns, code blocks) are reported as `rename_warnings`, not rewritten. Implementation:
`escurel_md::rewrite_page_kind` + `Indexer::migrate_kind_renaming`; the page, its open drafts, its historical
snapshots, the lake publish and the quarantine check all see the renamed pages.

## How to recognise it next time

A dry run whose `conflicts` lists pages that carry a legacy `type:` plus a `kind:` whose value is NOT
`skill`/`instance` (`kind: bus`). `grep -rn '^kind:' markdown/instances | grep -v 'kind: \(skill\|instance\)'`
finds them offline. Before the window, run the dry run WITH the rename and read `rename_warnings`.

## Changelog lines for the release notes

- `escurel admin migrate-kind --rename-field OLD=NEW` (repeatable) and the MCP `migrate_kind` parameter
  `rename_fields` rename a tenant's own top-level frontmatter field that collides with the page-kind key
  (e.g. `kind=system_kind`) inside the `type:` -> `kind:` migration: on every page that still has the legacy
  `type:`, in the declaring skills' `optional_frontmatter`/`required_frontmatter`, in open drafts and historical
  snapshots. A colliding target is reported (`rename_collisions`, also in `conflicts`), other mentions of the old
  name are listed in `rename_warnings` and not rewritten. `migrate-kind-job` takes the pairs from
  `ESCUREL_MIGRATE_KIND_RENAME` (comma-separated) for the dry run, the apply and the verification dry run.
