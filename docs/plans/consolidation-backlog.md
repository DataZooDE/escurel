# Consolidation backlog (post ad-hoc period, 2026-10-09)

Branch triage from the wave-1 cleanup. Every remote branch whose content is in `main`
(by `git cherry`, by a merged PR, or as an ancestor of the #660 squash source `7aea3dcc`)
was deleted on 2026-10-09: 59 branches. What follows is the remainder.

## Keep

| Branch | Why |
|---|---|
| `release/0fb7f015-anofox-optimize` | The Dockerfile bake of `anofox_optimize` (+44/-2) is not on `main`; lands as PR `ci/bake-extensions-dockerfile`, then the branch goes. The hetzner lab image `0fb7f015-opt1` was built from it. |

## 2-week stay: delete on 2026-10-23 unless claimed

None of these has a merged PR; the diffstat is against the merge base with `main`.

| Branch | Last commit | Diff vs main | PR | Judgement |
|---|---|---|---|---|
| `feat/agent-context-query` | 2026-09-12 | 2 files, +348 | #495 closed | A spike (one commit) that was closed, not merged. Nothing on `main` references it. |
| `feat/quack-serving-runtime` | 2026-09-12 | 2 files, +512 | #493 closed | Spike for a quack serving runtime; closed unmerged. |
| `feat/draft-versions` | 2026-08-14 | 15 files, +1447 | #396 closed | Draft versioning; superseded by the drafts work that landed in #487/#494/#654 (`versioning_unavailable` + draft `base_version`). Check for a unique test before deleting. |
| `demo/2-seed-path` | 2026-05-29 | 31 files, +2292 | #83 closed | May 2026 demo seed path; `seed_from_dir` on `main` covers it (15 files). Superseded. |
| `release/v1.0.0-ci` | 2026-05-28 | 14 files, +309/-158 | none | The v1.0.0 CI re-enable work; `main` carries the live CI since v1.0.0. Historical only. |

Reviewed and deleted on 2026-10-09 because their PR was merged (the `+` lines `git cherry`
showed were squash artifacts): `feat/evolve-scenarios-view` (#658), `feat/anofox-evolve-integration`
and `anofox-evolve-integration` (#653), `vscode/m3-wave2-seam` (#606), `feat/runner-terminal-drive`
(#458), `claude/issue-357/369/374` (#367/#370/#375), `fix/424-…` (#424), `ci/release-job-cache` (#428).

## Local worktrees kept on purpose

| Path | Why |
|---|---|
| `~/Projects/datazoo/build-1.5.6/escurel-main` (`feat/evolve-chart-images`) | Unmerged `ESCUREL_E2E_SLOW` work; extracted in the wave-3 CI PR. |
| `~/wt-escurel/opt1` (`release/0fb7f015-anofox-optimize`) | See above. |
| `~/escurel-web-deploy` (detached `7aea3dcc`) | The live web workbench stack; re-deployed from the release tag in wave 4. |
