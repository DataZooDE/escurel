# DuckDB `LOAD <ext>` needs prior `INSTALL <ext>` on a fresh host

**Date:** 2026-05-24
**Scope:** `escurel-index` schema migrator (CI failure on PR #6)

## Symptom

Migration succeeded locally but failed on the GitHub Actions
runner with:

```
IO Error: Extension "/home/runner/.duckdb/extensions/v1.5.3/
linux_amd64/vss.duckdb_extension" not found.
Extension "vss" is an existing extension.

Install it first using "INSTALL vss".
```

The migration SQL had `SET autoinstall_known_extensions = true;`
followed by `LOAD vss;` — and `LOAD` was failing on a host with
no `~/.duckdb/extensions/` cache.

## Cause

`autoinstall_known_extensions` does **not** trigger on a bare
`LOAD <extension>` statement. It triggers when the binder
encounters a function or type reference that needs an extension
to resolve, and that extension is in DuckDB's "known" set.

`LOAD vss` against a host where vss has never been installed
fails with the IO error above; the autoinstall hook is bypassed
because we asked for a load, not a reference resolution.

This was hidden locally because my Arch laptop has
`~/.duckdb/extensions/v1.5.3/linux_amd64/vss.duckdb_extension`
cached from previous DuckDB use unrelated to Escurel. The CI
runner had no such cache.

## Fix

Be explicit: `INSTALL <ext>; LOAD <ext>;` for every extension we
depend on. `INSTALL` is idempotent (no-op if already installed),
so the migration runs the same way on a fresh CI runner and a
populated dev laptop.

```sql
SET autoinstall_known_extensions = true;   -- still set, for
SET autoload_known_extensions    = true;   -- any function-style
                                           -- references later.
INSTALL vss;
LOAD vss;

INSTALL fts;
LOAD fts;
```

## How to recognise next time

If a DuckDB extension errors with "Extension not found" + "Install
it first using INSTALL <ext>" on CI but works locally — your
local cache is masking a missing INSTALL in your migration.
Always prefer explicit `INSTALL` over relying on autoinstall for
extensions that are part of your guaranteed-runtime schema.

## Lesson

Per the engineering principle of "no-mock integration tests" —
they ran successfully locally but CI exposed the cache-dependency
that mocked-out the autoinstall behaviour. Without CI as an
independent runner, this bug would have shipped silently. The
test was honest; the local environment was lying.
