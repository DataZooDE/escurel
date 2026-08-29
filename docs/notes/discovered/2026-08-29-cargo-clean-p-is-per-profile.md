# `cargo clean -p <pkg>` is per-profile, and it cost us the release cache

*2026-08-29*

## Symptom

`.github/workflows/ci.yml`'s `build --release` job ran with **no cache at
all**, and the comment explaining why was confident and detailed:

> NO rust-cache here — deliberately, and NOT an oversight. […] The prior
> `cache-directories: target/duckdb-download` + conditional `cargo clean -p
> libduckdb-sys` safety step did NOT close this (the release job stayed red
> on main) […] A cold release build per PR is the price of a green gate.

The price was 23m36s on every pull request — the longest job in the run,
and after the test suite was fixed, the thing setting the wall clock.

The underlying failure it describes is real. libduckdb-sys downloads
libduckdb into `target/duckdb-download` and links against it;
`Swatinem/rust-cache` prunes that `.so` when it saves, even though it is
listed in `cache-directories`. A restored cache therefore brings back the
build-script *fingerprint* — so the script does not re-run and does not
re-download — while the library it points at is gone, and the link fails
with `unable to find library -lduckdb`.

The safety step exists precisely to break that: if the `.so` is missing,
`cargo clean -p libduckdb-sys` forces the script to re-run. In the `ci` job
it works. In the `build --release` job it did nothing at all.

## Cause

**`cargo clean -p <pkg>` cleans the dev profile only.** It needs `--release`
to touch `target/release`. Measured on this workspace with both profiles
built:

```
$ cargo clean -p libduckdb-sys --dry-run -v | grep -oE 'target/(debug|release)' | sort | uniq -c
     58 target/debug

$ cargo clean -p libduckdb-sys --release --dry-run -v | grep -oE 'target/(debug|release)' | sort | uniq -c
     48 target/release
```

The `ci` job builds dev/test artifacts, so the bare command matched its
profile and the cache worked. The `build --release` job builds release
artifacts, so the identical line was a guaranteed no-op: the stale
fingerprint survived, the `.so` stayed missing, and the job went red on
every warm cache.

Two things then compounded it. The step is conditional and prints nothing
when it does not fire, so a no-op and a not-needed look the same in the log.
And the failure only appears on the *second* run — the one with a cache —
so it never reproduces when you push a fix and watch the first run go green.

## The fix

Add `--release` to the safety step in that job, and put the cache back:

```yaml
- uses: Swatinem/rust-cache@v2
  with:
    shared-key: release-build          # its own key; different profile to `ci`
    cache-directories: target/duckdb-download
    cache-on-failure: true

- name: Ensure libduckdb is present (cache-restore safety)
  run: |
    if ! ls target/duckdb-download/*/*/libduckdb.so >/dev/null 2>&1; then
      cargo clean -p libduckdb-sys --release || true
    fi
```

## Verifying it

A cache fix cannot be verified by the run that ships it: the first run after
any key change is cold, and a cold cache always links. Verification took two
runs on the same PR branch, with the second commit touching no `Cargo.toml`,
`Cargo.lock` or `rust-toolchain.toml` so the rust-cache key was unchanged:

| run | cache | `build --release` |
|---|---|---|
| 1 | cold (saved `v0-rust-release-build-…`, 624 MiB) | 23m18s |
| 2 | restored | see below |

## How to recognise it next time

- **`cargo clean -p` takes a profile.** So do `cargo clean` and most of the
  `-p`-shaped commands. If a cleanup step runs in a job that builds
  `--release`, the step needs `--release` too. `--dry-run -v` prints exactly
  what a clean would remove and takes a second to run.
- **A conditional step that prints nothing on the happy path hides a
  no-op.** Echo on both branches, or the log cannot distinguish "did not
  need to run" from "ran and did nothing".
- **Cache bugs only reproduce on the second run.** A first run after a key
  change is always cold and always green. Verify a cache fix by triggering a
  second run with the cache key unchanged — for this repo, any commit that
  does not touch a `Cargo.toml`, `Cargo.lock` or `rust-toolchain.toml`.
- **Distrust a long comment that justifies removing a safety mechanism.**
  This one was accurate about the mechanism and wrong about the diagnosis,
  which is the combination that survives review: every checkable claim in it
  held up, and the conclusion still did not follow.
