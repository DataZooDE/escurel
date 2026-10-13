# The first tag build would have died at the Cargo.lock refresh (empty CARGO_HOME)

**Symptom.** Found by the second crew review, reproduced locally: on a release runner nothing is cached
(rust-cache is deliberately off, see `release.yml`), so `CARGO_HOME` is empty. The tag-only step "Refresh
workspace versions in Cargo.lock" runs `cargo update --workspace --offline`; with nothing fetched it resolves
against an empty registry and exits 101 (`no matching package named serde_yaml_ng found`). All three OS jobs
fail before any GitHub Release exists. A `workflow_dispatch` dry run never saw it, because that step is tag-only.

**Fix.** A `cargo fetch --locked` step runs first, BEFORE the tag-version injection (the manifests still match
the committed lockfile there, so `--locked` is valid). The lockfile refresh then works offline and its guard
(only `version = "..."` lines may change) is unchanged. Verified locally with an empty `CARGO_HOME`: fetch,
inject `2026.10.14`, `update --workspace --offline` (23 version lines, nothing else), `cargo metadata --locked`.

**Also in the same file** (cheap crew SHOULD items): a lightweight tag is refused (`git cat-file -t` must say
`tag`: the tag message is the release notes); a same-day re-cut `vYYYY.MM.DD.N` is normalised to
`YYYY.M.D+N` (SemVer build metadata) because Cargo accepts only three numeric components.

**Recognise it next time.** A tag-only step that needs the network or a populated cache passes every dry run.
Rehearse tag-only steps locally with an empty `CARGO_HOME`, or add a `simulate_tag` dispatch input.

## Changelog lines for the release notes
- CI/release: the tag build fetches dependencies before refreshing the lockfile (the first tag build would have
  failed on a cold runner); lightweight tags are refused; same-day re-cuts `vYYYY.MM.DD.N` build as `YYYY.M.D+N`.
