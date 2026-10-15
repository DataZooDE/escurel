# The lab cutover failed three ways that two clean rehearsals did not catch (2026-10-15)

## Symptom

The real lab move to escurel `c9b035c1190e` (hetzner #950) failed three times in a row, each time at a step the two
rehearsals on a Cloud SQL clone had passed:

1. **Attempt 1, before any SQL:** `escurel-server lake-upgrade` died with
   `IO Error: Can't find the home directory at '/tmp/escurel'`. The Job's `/tmp` is a fresh emptyDir; DuckDB's home
   (`ESCUREL_SERVER_DATA_DIR` / `HOME`) only existed where a server had booted before. Both rehearsal containers
   had it. Fixed in #719 (`lake-upgrade` and the server boot create the directory themselves).
2. **Attempt 2, after the lake had been re-pinned to schema 10:** the dry run answered
   `http 401 missing Authorization: Bearer header`. The lab Deployment sets `ESCUREL_AUTH_OIDC_ISSUER` and the job's
   loopback throwaway server inherited it while the job's CLI sent no token. The rehearsals ran with no OIDC.
   `helm --atomic` then rolled back to the previous image, which refuses lake schema 10, so dz-escurel was down
   (CrashLoopBackOff) from about 09:38 to 11:23 UTC until attempt 3 rolled FORWARD (a hook workaround that unset
   the auth variables). Fixed in #720 (the job drops every `ESCUREL_AUTH_*`).
3. **The ordering made (2) an outage:** `lake-upgrade` was the first step, and it is one-way for the previous
   release. A job that fails afterwards on config, auth or a conflict strands the old image. Fixed in #720: the
   one-way re-pin runs LAST (boot, dry run, apply, verify, `lake-upgrade`), and `lake-upgrade --to N` walks a
   compatible bump back.

Related engine gaps found by the first rehearsal and fixed before the cutover: the lake had no upgrade path
(#716), `migrate-kind --apply` left the lake stale and a stale lake was served silently (#718).

## How to recognise it next time

- The job fails within seconds with a DuckDB home-directory error: a fresh volume.
- The dry run reports `http 401 ... missing Authorization`: the throwaway server inherited the auth environment.
- The previous image crash-loops with `lake schema_version 10 != reader schema_version 9` after a failed job: the
  one-way step ran before the failure. `escurel-server lake-upgrade --to 9` (writer stopped) restores the pin ONLY if
  `migrate-kind --apply` has not run; once the apply has run the lane pages are `kind:` too and the way back is the
  backup (Cloud SQL point-in-time restore + lanes copy), or roll forward.

## The rule for future rehearsals

Rehearse in the SAME environment shape as the real run, not a convenient one:

- a fresh volume / empty emptyDir, not a container that has booted before;
- the lab's real environment including OIDC (issuer, JWKS) and the Job's `envFrom` secrets;
- the `helm --atomic` rollback path, and the PREVIOUS image started against the post-failure state;
- the exact Job command the chart hook runs.

And order any step that the previous release cannot undo LAST.

## Changelog lines for the release notes

- Covered in the root `CHANGELOG.md` entry "Lab-move engine fixes (#716, #718, #719, #720)".
