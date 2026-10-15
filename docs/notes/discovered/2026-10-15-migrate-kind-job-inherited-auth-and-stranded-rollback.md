# The migration job inherited the Deployment's OIDC, and re-pinned the lake before anything could fail

Date: 2026-10-15. Found by the real lab cutover (attempt 2, image `c9b035c1190e`).

## Symptom

`escurel-server lake-upgrade` succeeded (lake pinned to schema 10), then the job's `migrate-kind` dry run died:

```
"error": "http 401: {\"error\":\"unauthorized\",\"message\":\"missing Authorization: Bearer header\"}"
migrate-kind-job: FAILED: migrate-kind dry run failed
```

`helm --atomic` rolled back to the previous release, which **refuses** a lake pinned at schema 10, so `dz-escurel`
went into CrashLoopBackOff until the cutover was rolled forward by hand.

## Cause

Two independent faults that compounded:

1. The Job runs the Deployment's environment, which sets `ESCUREL_AUTH_OIDC_ISSUER` (+ the JWKS URI). The job's
   throwaway server (loopback, no Service) inherited it while the job's own CLI calls carry no bearer.
2. `lake-upgrade` ran FIRST. Nothing needs it first: a writer boot adopts a declared-compatible older lake as-is
   (`Migrator::LAKE_ADOPTS_FROM`). The pin is the one step the previous release cannot live with, so a failure
   after it stranded the rollback target.

What flips the lake to the new version: `lake-upgrade`, and every publish (`publish_lake` and the scoped publish
re-pin `escurel_manifest` to `Migrator::SCHEMA_VERSION`), including the one `migrate-kind --apply` ends with.
After the apply the lane content is `kind:` as well, so the previous release cannot serve it either: from there
the way back is the backup, not a re-pin.

## Fix

- The job drops every `ESCUREL_AUTH_*` variable (indexed ones too), `ESCUREL_WEBHOOK_*`, and runs its server with
  `ESCUREL_SNAPSHOT_PUBLISH_SECS=0` (a periodic publish would re-pin the lake on its own timer) and no metrics
  listener. Threat model: loopback only, a Job pod with no Service and no ingress, the job's CLI is the only caller.
- Order: boot -> dry run -> apply (publishes) -> second boot (not quarantined, nothing left) -> `lake-upgrade` LAST.
  Everything that can fail on configuration, auth or a conflict now fails while the previous release still boots.
- `escurel-server lake-upgrade --to <N>` walks the pin back for a declared-compatible version only (idempotent,
  refuses everything else, same stopped-writer lease).

## Recognise it next time

A job that exits right after `lake-upgrade` printed `"upgraded"` with `http 401`; or a rolled-back pod logging
`lake schema_version 10 != reader schema_version 9`. Recovery: stop the writer, run the new image's
`escurel-server lake-upgrade --to 9`, redeploy the previous image.

## Changelog lines for the release notes

- `migrate-kind-job` no longer inherits the Deployment's `ESCUREL_AUTH_*` (or webhook / periodic publish / metrics
  listener) environment: its loopback-only throwaway server runs without auth, so an OIDC-enabled gateway can be
  migrated with the same pod environment.
- `migrate-kind-job` now takes the one-way lake re-pin LAST (boot, dry run, apply, verify, then `lake-upgrade`), so a
  job that fails on config, auth or a conflict leaves the lake where the previous release can still boot it.
- New `escurel-server lake-upgrade --to <N>`: walks the lake manifest pin back to a declared-compatible older schema
  version (a failed cutover must not strand the previous release); idempotent, refuses anything else.
