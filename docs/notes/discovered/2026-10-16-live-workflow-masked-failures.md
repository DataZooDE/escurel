# The nightly `live` workflow was red for weeks and each red hid the next one

## Symptom

`live` (storage + DuckLake against real containers) failed every night from at least 2026-09-10. Fixing the first
cause (the MinIO image is gone from Docker Hub and then from quay.io, #726) did not make it green: the next step
failed, and behind that were more steps that had not run for weeks.

## Why it hid

GitHub Actions skips every later step after a failure unless the step says `if: ${{ !cancelled() }}` (or
`always()`). The first step that needs a container image failed to pull, so the reader-role tests, the Postgres
connector tests, the writer-lease tests and the SIGKILL matrix never ran. A test that broke in that window stayed
broken without anyone seeing it.

## The failure that was waiting behind the MinIO one

`reader_role_crdt::reader_no_longer_rejects_session_tools_when_ducklake_configured` failed twice over, both
test-setup regressions, neither a product bug:

1. `ESCUREL_WRITE_ACL` defaults to `enforce` since #655. The test builds an embedded reader from an injected map and
   calls with no credentials (`anonymous`), which `enforce` refuses by design (`open_session denied: caller
   anonymous may not open a session on ...`). The ACL mode was read straight from the **process** environment
   (`WriteAclMode::from_env()` inside `EscurelConfig::build`), so the injected map could not set it, and a
   process-global `set_var` in a suite binary of 800+ parallel tests is not an option. `EscurelConfig::from_source`
   now reads `ESCUREL_WRITE_ACL` from the config source like every other key (`EscurelConfig.write_acl`); production
   behaviour is unchanged because `from_env` uses the process environment as the source and an unset or unknown
   value still enforces.
2. The test committed the bare text `hi from a reader`. `close_session {commit: true}` writes the merged body through
   as a page, and a page must start with frontmatter, so the commit failed with `close_session write-through:
   markdown parse error: missing frontmatter` (JSON-RPC -32603). The test now commits a minimal valid page.

## Fix

- `live.yml`: every test step runs with `if: ${{ !cancelled() }}`, so one red step no longer hides the others.
- The two test-setup fixes above.
- Verified locally the way the workflow runs them: reader-role chat/events/crdt, `sql_rows_postgres` (13),
  `sql_view_postgres`, `writer_lease_live`, and the full `migrate_kind_sigkill` matrix (`ESCUREL_SIGKILL_FULL=1`, ~6 min).

## How to recognise it next time

A scheduled workflow that is red "every morning" and only ever shows the first failure: open the step list and look
for steps that are **skipped** after the red one. Skipped is not green. Add `if: ${{ !cancelled() }}` to independent
steps.

## Observation, not changed

An invalid page body committed from a session surfaces as an internal error (-32603) rather than a typed,
caller-facing error. The caller cannot tell their own bad input from a server fault. Left for a later change.

## Changelog lines for the release notes

None: test and CI only. `EscurelConfig` gains the public field `write_acl`; it is only constructed through
`from_source`, so no caller is affected.
