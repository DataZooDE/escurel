# Release checklist (engine `vYYYY.MM.DD` + skill + VSIX)

Escurel releases are cut by pushing an **annotated** `v*` tag. `release.yml` then builds `escurel` +
`escurel-server` for linux-amd64 / windows-amd64 / macos-arm64 (smoke-tested, libduckdb bundled), the
project-memory variants (linux + windows), the VS Code VSIX, and publishes the GitHub Release with
`SHA256SUMS`. `publish-image.yml` builds `ghcr.io/datazoode/escurel:<tag>` and
`ghcr.io/datazoode/escurel-runner:<tag>` from the same tag (the server image with `REQUIRE_GDRIVE=1`: a
missing gdrive artifact on the mirror fails the release instead of dropping the extension).
**Nothing here is automatic until the tag exists: tagging and VSIX publication are an explicit owner decision.**

Versions follow `vYYYY.MM.DD` (the date of the cut; a same-day re-cut appends `.N`). Cargo and the VSIX get
the SemVer-normalised number (`2026.10.14` → `2026.10.14`; leading zeros stripped per component).

## 1. Before the tag

1. **main is green**: the last commit on `main` has a green `fmt + clippy + test` check (the release job
   refuses a tag whose commit has none; emergency escape: repository variable `RELEASE_SKIP_CI_GATE=1`, unset it
   afterwards) and no open must-fix PR. `git fetch && git log --oneline origin/main -5`.
2. **Skill and CHANGELOG final**: `.claude/skills/escurel-platform/VERSION` and its `CHANGELOG.md` say what
   shipped; in the root `CHANGELOG.md` rename `## Unreleased — BREAKING (…)` to `## vYYYY.MM.DD — BREAKING (…)`
   (the release job fails if the CHANGELOG has no `## <tag>` section) and open a new empty `## Unreleased`.
   Merge that as a docs PR (title `docs: CHANGELOG vYYYY.MM.DD`).
3. **Dry run** the build on `main`: `gh workflow run release.yml --ref main`, then `gh run watch`. A
   `workflow_dispatch` run builds, smoke-tests and packages only (artifacts, **no Release**). All three
   platforms plus `build vsix` must be green. (`create-release` only runs on tags.)
4. **Migration runbook still true**: `docs/deploy/kind-migration.md` (stop-first upgrade, backup, rollback) and
   `scripts/migrate-kind-job.sh`.

## 2. The tag (owner go required)

Draft the annotated tag message from the CHANGELOG section. It MUST contain the word `BREAKING` when the
section does (the release job checks it). Suggested opening paragraph for the first release after the
`kind:` cut:

> **BREAKING.** Page frontmatter `type: skill|instance` is now `kind:` (wire `page_type` → `page_kind`); existing
> tenants boot quarantined until `escurel admin migrate-kind` has run (see docs/deploy/kind-migration.md).
> `ESCUREL_WRITE_ACL` now defaults to enforce. MCP `tools/list` moves `execution` to `_meta.escurel.execution`.
> `ESCUREL_CURSOR_KEY` must be at least 32 bytes. Secrets are per tenant (`ESCUREL_SECRET_<TENANT>__NAME`).
> MySQL is not supported (refused, not advertised). DuckDB is 1.5.6: every DuckDB extension must be a v1.5.6
> build (anofox_optimize and gdrive are published on get.erpl.io under `v1.5.6/linux_amd64/`).

```sh
TAG=vYYYY.MM.DD
git fetch origin && git checkout --detach origin/main      # the commit that passed the dry run
git tag -a "$TAG" -m "$(sed -n "/^## $TAG/,/^## /p" CHANGELOG.md | sed '$d')"
git push origin "$TAG"          # if SSH is unavailable: git push https://github.com/DataZooDE/escurel.git "$TAG"
```

## 3. After the tag

1. Watch both workflows: `gh run list --limit 5` (`release`, `publish-image`).
2. `gh release view "$TAG"` lists: 3 platform archives, 2 project-memory archives (linux, windows), the
   `escurel-<ver>.vsix`, and `SHA256SUMS` (`sha256sum -c SHA256SUMS` on the downloaded files).
3. Image: `docker manifest inspect ghcr.io/datazoode/escurel:$TAG` and `…/escurel-runner:$TAG`; the server image
   contains gdrive (build log has no `gdrive … WITHOUT` warning block).
4. **VSIX**: the GitHub Release carries the file. Publishing it to a marketplace (VS Marketplace / Open VSX)
   needs the owner's explicit go and a publisher token; it is not part of `release.yml`.
5. **Consumers** (each moves in the same window as the engine; see `docs/deploy/consumer-rollout.md`):
   - hetzner lab: escurel image `:$TAG` with anofox_optimize + gdrive 1.5.6, agent-template (PR #359, writes
     `kind:` pages) in the same window, then `escurel admin migrate-kind` per tenant
     (`docs/deploy/kind-migration.md`); the lab owns its values PR (ADR-0016).
   - heron (pins agent-template), anofox-evolve (Cargo `rev` → the release commit), datazoo-ai-engineering
     (follows the checkout; push its pending commits).
6. Update the memory notes and close the release PR train.

## Known deltas and decisions

- Shipped binaries are built with `--features s3,gcs,duckvfs` (+ default `kreuzberg`, `gemini`); the container
  image with `gemini,s3,gcs,duckvfs`. The only difference left is `kreuzberg` (default-on in the binaries;
  see `Dockerfile` for the image).
- No `libduckdb`/extension signing: extensions from the erpl mirror load unsigned
  (`ESCUREL_ALLOW_UNSIGNED_EXTENSIONS`), as today.
