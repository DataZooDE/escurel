# The test suite spent 70% of its CPU generating RSA keys

*2026-08-29*

## Symptom

CI's `cargo test --workspace --all-targets` step took **24 minutes**. The
obvious reading was that the suite was too big, or that `libtest`'s
one-binary-at-a-time scheduling was starving a 2-core runner of parallelism.

Both readings were wrong, and the measurement that settles it is one number:

```
cargo nextest run --workspace     # 32-core box, warm tree
  Summary [122.138s] 1357 tests run: 1357 passed
  real 2m3s   user 54m6s
```

**122 seconds of wall clock, 54 minutes of CPU.** A suite that burns 54
CPU-minutes takes ~24 minutes on two cores no matter how well it is
scheduled. The suite was not badly parallelised, it was doing far too much
work — so the runner swap everyone reaches for first was the smaller half of
the fix.

Per-binary CPU pointed straight at the culprit:

| binary | tests | CPU | per test |
|---|---|---|---|
| `escurel-server::suite` | 451 | 41.6 min | **5.5 s** |
| `escurel-index::suite` | 232 | 3.3 min | 0.85 s |
| `escurel-auth::multi_issuer` | 4 | 1.2 min | **18 s** |

`escurel-index` boots the same DuckDB and the same indexer. The 6x gap is
everything `escurel-server` does *on top*: `EscurelProcess::spawn`. And
`multi_issuer` — four tests that do nothing but verify a JWT — taking 18
seconds each is not a plausible cost for verifying a JWT.

## Cause

`escurel-test-support`'s in-process OIDC issuer signs its JWTs with a
freshly generated 2048-bit RSA keypair, one per `EscurelProcess::spawn`
with `AuthMode::TestIssuer` — 163 call sites, ~400 spawns.

Tests build with the **`dev` profile**, and nothing in the root
`Cargo.toml` optimised dependencies. So `rsa` and `num-bigint-dig` ran
their bignum arithmetic at `opt-level = 0`. Benchmarked against the pinned
versions:

| build | RSA-2048 keygen |
|---|---|
| deps at `opt-level = 0` | mean **4.88 s**, median 3.01 s, max 9.73 s |
| deps at `opt-level = 3` | mean **0.23 s** |

The doc comment on `Keys::generate` had said keygen "keeps key generation
under ~200 ms in release builds". That was true, and it was the trap: no
test has ever run it in a release build. A statement about the profile you
are *not* using reads exactly like a statement about the one you are.

## The fix

Four lines in the root `Cargo.toml`:

```toml
[profile.dev.package.rsa]
opt-level = 3

[profile.dev.package.num-bigint-dig]
opt-level = 3
```

Measured across the whole workspace, that alone:

| | before | after |
|---|---|---|
| suite CPU time | 62.2 CPU-min | **19.4 CPU-min** |
| `escurel-server::suite` | 5.5 s/test | **1.41 s/test** |
| wall clock (32 cores) | 122 s | 57 s |

`Keys` is now also memoised per process (`Keys::shared`), which removes the
rest of the cost wherever one process runs many tests. Under `cargo
nextest` — one process per test — that residual is ~0.2 s per test and it
is the floor; buying it down further would mean committing a private key to
the repo.

## The runner swap that did not pay

The reflex fix for "many test binaries, slow CI" is `cargo nextest`, and it
was tried. On a 32-core workstation it halves the wall clock (100s -> 50s),
because libtest runs binaries one after another and nextest schedules across
them.

On the runner CI actually uses, it loses. nextest runs each test in its own
**process**, so `Keys::shared`'s per-process memoisation buys nothing — the
keygen comes back once per test. That is a CPU-for-scheduling trade, and a
2-core runner has no spare CPU to trade with. Pinned to two cores:

| runner | wall | CPU |
|---|---|---|
| `cargo test --workspace --all-targets` | **4m49s** | 6m41s |
| `cargo nextest run --workspace` | 5m46s | 8m41s |

CI therefore stays on libtest, and `.config/nextest.toml` exists for local
use only. The general lesson is the same as the one above: *this suite is
CPU-bound*, and on a CPU-bound suite a better scheduler is not a fix — it
can be a regression, if the scheduling is bought with CPU.

## How to recognise it next time

- **Measure CPU-time, not wall-clock, when the target is a small CI
  runner.** `user` from `time` is the number that predicts a 2-core runner.
  A profile that looks fine on a 32-core workstation hides a 30x
  amplification.
- **Compare per-test cost between two binaries that share a backend.** The
  cheap one is your baseline for what the expensive one *should* cost; the
  gap is a specific thing you can name.
- **A dependency doing real computation in a test build is running
  unoptimized.** Crypto, compression, image decode, bignum — check for a
  `[profile.dev.package.<dep>]` override before assuming the test is slow.
- **Distrust performance claims in comments that name a profile.** If a
  comment says "in release builds", ask which profile the code under
  discussion actually builds with.
- **Benchmark the CI runner, not the workstation.** `taskset -c 0,1` in
  front of the command reproduces a 2-core runner well enough to reverse a
  conclusion — it reversed this one.
