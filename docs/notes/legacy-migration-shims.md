# Legacy migration shims (remove after v2027.xx)

The `type:` -> `kind:` hard cut (skill 0.8.0) left code that exists only so a store written before the
cut can be carried across. None of it is part of the steady-state design; delete it once no tenant
can still hold pre-cut data.

| shim | where | what it does |
|---|---|---|
| `rewrite_legacy_type_key`, `rewrite_workflow_run_status` | `crates/escurel-md/src/legacy.rs` | the pure text edits `migrate_kind` applies to a page |
| `ParseError::LegacyTypeKey` | `crates/escurel-md/src/lib.rs` | refuses a page that still says `type: skill|instance`, naming the migration |
| `escurel admin migrate-kind`, the `migrate_kind` tool | `escurel-index/src/migrate_kind.rs`, server `tools_admin.rs`, CLI | rewrites pages, open drafts, CRDT snapshots; dry-run by default |
| quarantine boot, the durable `meta/migrate-kind.pending` marker | `escurel-index`, `escurel-server` | a tenant with legacy pages serves only the migration until it is done |
| `frontmatter_type_removed` issue | `escurel-index/src/validate.rs` | a write that still carries `type:` is refused with the exact fix |

Removal checklist: confirm every production tenant reports `escurel_tenant_quarantined 0` and
`escurel_migration_pending 0`, delete the rows above, keep `frontmatter_type_removed` for one more
release so a stale agent prompt gets a clear error instead of a generic one, and drop the
`docs/deploy/kind-migration.md` runbook last.
