# A tag name reached shell `run:` blocks unvalidated; security review of the demo/workbench/release scripts

## Symptom / finding
`release.yml` derived the version from `GITHUB_REF` (`refs/tags/v*`) and interpolated it with
`${{ steps.ver.outputs.version }}` into `run:` shell text (staging, archiving, `npm version`, `vsce package`). A git tag
name may contain `$`, `(`, `;`, a backtick, `"`, `|`, `&`, so a tag like `v1$(cmd)` would run `cmd` in a release job that
holds `contents: write`. Needs push access to create the tag (so defence in depth, not an outside attack), but the job
holds the write token.

## Fix
`scripts/release-version.sh` is the only place the version is derived and accepts only digits and dots, an optional
`-prerelease` and an optional `+build` (`[0-9A-Za-z.]`). Both `ver` steps call it. `scripts/test-release-version.sh`
(14 cases, red first) runs in the `fmt + clippy + test` CI job.

## Review of the other scripts (Muse hit its quota until 2026-10-19, so this was done by hand; each point read in code)
| area | result |
|---|---|
| `scripts/release-gate.sh` | SHA validated as 40 hex before use; every `gh`/`jq` argument quoted. No finding. |
| `scripts/migrate-kind-job.sh` | `ESCUREL_TENANT` is quoted everywhere, the temp file is `mktemp`, no `eval`. No finding. |
| `deploy/web-workbench/entrypoint.sh`, compose | argon2 hash preferred, plain password length/placeholder checked, container `read_only`, `cap_drop: ALL`, `no-new-privileges`, port published on `127.0.0.1` by default. Plain `PASSWORD` stays visible in `docker inspect`: LOW, already documented ("prefer HASHED_PASSWORD"). |
| gateway in the demo compose | unauthenticated and `ESCUREL_WRITE_ACL=off` on purpose, reachable only on the compose network (not published). Documented; not a finding while it stays unpublished. |
| `editors/vscode/demo/web/story.sh` `spin` | `eval "$1"` of a condition built from `$S` (state dir) and a port taken with a digits-only `sed`. The inputs are operator-controlled env, not data. LOW, left as is. |
| `deploy/web-workbench/s2d-up.sh` | `chmod -R a+rX` on the S2D data dir so the uid-65532 gateway can read it; the data is the synthetic S2D seed. LOW. |
| GitHub Actions | no `pull_request_target`, no PR title/branch text in `run:`; the only interpolated expression of untrusted origin was the tag version (fixed). Release job permissions are minimal (`contents: read`, `write` only on the release job). |

## S8 (write-back watchdog vs COMMIT): analysed, no change
`with_statement_timeout` wraps the whole `apply` including `COMMIT`, so a slow COMMIT can be interrupted at the deadline.
That is reported as a transient `backend_unavailable`, and the SQL write-back path already recovers from a commit that
landed anyway: after a transient failure it looks with `rows_holds_patch` before resending, and a later promote checks
`already_there` before the etag comparison, so it answers `applied` rather than `write_back_conflict`. The REST path has
the per-attempt audit rows from #705. No clean small fix remains; left for later: a Postgres-level test with an injected
slow COMMIT (needs a controllable source).

## Changelog lines for the release notes
- Security: the release workflow now validates the version taken from a tag name (digits and dots, optional
  `-prerelease` / `+build`) before it reaches any shell step; a tag with shell metacharacters is refused.
