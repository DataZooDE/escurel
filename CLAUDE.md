# How we work on escurel

This file is the working contract between contributors (including AI
assistants) and this codebase. Read it before opening a PR.

The v1 specification lives under [`docs/`](docs/) — start at
[`docs/README.md`](docs/README.md). This file is *not* a re-statement of
the spec; it captures the engineering principles for how we turn the
spec into running code.

## Nine principles

1. **Red → green TDD.** Every code change starts with a failing test
   that names the target behaviour. No code without a test that would
   have caught its absence. The order is non-negotiable: red first,
   green second, refactor third.

2. **A task is done when a no-mock integration test passes locally.**
   Unit tests are fine for the inner loop. The merge gate during
   rapid bootstrap is an integration test that exercises the *real*
   component — real filesystem, real DuckDB file, real S3 endpoint
   (MinIO testcontainer), real network where possible. No `mockall`,
   no test doubles at the boundary the test exists to cover. If you
   cannot exercise the real component from a test, the test is not
   yet finished.

   **CI policy.** GitHub Actions CI is **live** — re-enabled at
   v1.0.0 (M6). `.github/workflows/ci.yml` runs the
   `fmt + clippy + test + build` job on every `pull_request`, every
   `push` to `main`, and every `v*` tag; that job is the merge gate.
   The local four-command gate — `cargo fmt --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace --all-targets`, and `cargo build
   --workspace --release` — is still expected to pass before you push
   (it's the fast inner loop), but it is no longer the *only* safety
   net: CI re-runs the same checks on the PR. The DuckDB compile is no
   longer the tax it once was — libduckdb is downloaded precompiled
   (see `.cargo/config.toml`), so CI is cheap enough to run per-PR.

3. **12-factor.** Config via `ESCUREL_*` env vars (overriding TOML
   defaults); logs JSON to stdout; processes stateless except for
   explicit host-volume state; ports bound at startup (`8080` HTTP);
   graceful `SIGTERM`; backing services (LaneStore,
   OIDC issuer, OTel collector) are attached resources behind traits.

4. **Substrate alignment.** Match the
   [`substrate-platform`](file:///home/jr/.claude/skills/substrate-platform)
   skill's runtime contract (ADR-0013: Kamal + OpenTofu + private ghcr +
   GCP backplane): `/healthz` (liveness, dependency-free), `/version`,
   `/metrics`; secrets from **GCP Secret Manager** → env at deploy; the
   host-1 data **Volume** bind-mounted at `/data`; structured JSON logs to
   stdout (`ts`, `level`, `msg`, `app`, `env`, `version`, `request_id`) →
   Cloud Logging. escurel deploys as a **Kamal stateful pet** — pinned to
   host-1, **STOP-FIRST** (single-writer DuckDB). The image is this repo's
   `Dockerfile` → ghcr; the `kamal/dz-escurel/deploy.yml` + `apps/registry.yml`
   row live in the **substrate repo** (two-actor model). Nomad/Consul/Vault/
   Fabio/Packer are retired — see [`docs/deploy/`](docs/deploy/).

5. **SOLID + clean code.** Boundaries are traits (`LaneStore`,
   `Embedder`, `Reranker`, …); dependencies point inward; one Cargo
   crate per concern; public APIs are small, well-named, and
   minimally surprising. Prefer composition over inheritance,
   explicit over implicit, narrow over broad.

6. **Incremental PRs.** One logical change per PR; target under
   ~400 LOC diff. Each PR independently reviewable; merge only when
   local checks are green. Branch name convention:
   `bootstrap/<n>-<slug>` for the bootstrap sequence, then
   `<area>/<short-slug>` afterwards.

7. **Ask, don't assume.** When the spec is ambiguous, an external
   dependency is missing, or two locked decisions disagree, raise
   it as a question rather than picking. Surprises that get papered
   over compound; surprises that get asked about get resolved once.

8. **Future-notes for discovered problems.** When a non-obvious
   problem is fixed — a DuckDB extension gotcha, an S3-hostname
   trap, a Loro version pin, a CI-cache invalidation surprise —
   write a short note under
   [`docs/notes/discovered/`](docs/notes/discovered/) as
   `<YYYY-MM-DD>-<slug>.md` describing the symptom, the fix, and
   how to recognise it next time. We don't want to rediscover the
   same problem twice.

9. **Periodic codex reviews.** At natural pause points — a milestone
   landing, a new crate stabilising, the end of a multi-PR sequence
   — invoke a second-opinion review via OpenAI Codex CLI focused on
   **design**, **security**, **stability**, and **missing
   functions**. The earlier codex caught a path-traversal hole in
   `escurel-storage` (PR #7) that the merged tests missed; that's
   the failure mode this principle targets.

   ```bash
   # Review the diff since a known-good base, prompt via stdin.
   echo "Focus: design, security, stability, missing functions.
         Report MUST-FIX / NICE-TO-HAVE / OBSERVATION with file:line
         refs. Under 600 words." \
     | codex exec review --base <commit>
   ```

   Always `git status` after a codex run — `codex exec` runs full-
   auto by default and may write unrelated files (see
   [`docs/notes/discovered/2026-05-24-codex-full-auto-writes.md`](docs/notes/discovered/2026-05-24-codex-full-auto-writes.md)).
   Triage codex findings; the codex output is advisory, not a merge
   gate.

## What this looks like in practice

A PR cycle:

1. Branch from `main`.
2. **Write the failing test first.** Run it; confirm red for the
   right reason (not a compile error you didn't intend, not a
   missing fixture).
3. Implement the minimum to turn it green; rerun.
4. Local pre-push — all four must pass:
   - `cargo fmt --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace --all-targets`
   - `cargo build --workspace --release`
5. Push the branch; open a PR with **Summary** and **Test plan**
   sections. The test plan names the new integration test(s) by
   file + test function.
6. If the PR fixed a non-obvious problem, drop a note under
   `docs/notes/discovered/` in the same PR.
6b. **Re-sync `.claude/skills/escurel-platform` in the SAME PR** if the
   change touched anything a consumer sees — the tool surface, the CLI
   commands, the wire shapes, the binaries, the env vars, the data
   model. See *Keeping the consumer skill in sync* below. The skill is
   documentation that agents act on directly; stale entries are acted
   on as fact, which is worse than no documentation.
7. Merge with `gh pr merge --squash --delete-branch` once CI is
   green (CI is live — see principle 2).
8. **Reclaim the disk.** If the work happened in a worktree, remove
   it now that the branch is merged — `git worktree remove <path>`
   — and run `scripts/reclaim-disk.sh --all`. See *Disk discipline*
   below; this step is not optional housekeeping, it is the step
   that keeps the repo from reaching 845 GB again.

## Locked decisions (current bootstrap)

- **PR workflow:** feature branch → GitHub PR against `main` →
  local checks green → squash-merge. GitHub Actions CI is **live**
  (re-enabled at v1.0.0) and re-runs `fmt + clippy + test + build`
  on every PR/push as the merge gate (see principle 2).
- **`Cargo.lock` is committed.** The workspace has native deps
  (libduckdb-sys); pinning is the standard recommendation for any
  workspace that produces binaries or links native libraries.
- **License + advisory audit via `cargo deny check`** against the
  root `deny.toml` (permissive allow-list per
  [`docs/spec/roadmap.md § Licenses`](docs/spec/roadmap.md#licenses)).
  Run at milestones / dep freezes, not per-PR. See
  [`docs/deploy/README.md § License + advisory audit`](docs/deploy/README.md).
- **M1 acceptance:** our own spec-derived integration tests; no
  port of the Python prototype's 28-assertion suite (prototype not
  located at bootstrap time).
- **Substrate naming:** the substrate surface is `dz-escurel` /
  `datazoo-substrate-app-<env>/dz/escurel/` (the `apps-dz` Vault policy is
  gone with Vault). The binary surface stays `ESCUREL_*` / `escurel.*`. See
  [`docs/deploy/substrate.md § Naming convention`](docs/deploy/substrate.md).
- **Deployment concept (ADR-0013):** the Hetzner substrate is Kamal +
  OpenTofu + ghcr + a GCP backplane (two-actor PR model). The prior
  HashiCorp stack (Nomad/Consul/Vault/Fabio + Packer) is retired; its
  jobspecs/image fragments were removed from `docs/deploy/`. The per-app
  Kamal deploy contract + registry row live in the substrate repo.

## Keeping the consumer skill in sync

`.claude/skills/escurel-platform/` is **shipped documentation**, versioned
in this repo and symlinked into every consumer repo. Downstream agents
read it and act on it directly, without checking the source — so a stale
line is not a cosmetic problem, it is an agent confidently doing the
wrong thing. Stale docs are worse than absent docs.

It drifts silently because nothing compiles it. Treat it as part of the
public API: **if a PR changes what a consumer sees, it updates the skill
in the same PR.** Retrofitting it later does not happen.

What counts as consumer-visible:

| change | reference to update |
|---|---|
| a tool added/renamed/removed, or its inputs change | `02-tool-surface.md` (+ the tool list in `SKILL.md`'s `description`) |
| a CLI subcommand added/renamed | `04-consume-via-cli.md` (+ its CLI→tool map) |
| a new binary, or a change to how you run one locally | `09-local-iteration.md` |
| a new storage/index backend or env var a consumer sets | `09-local-iteration.md`, `07-fixtures-and-seeding.md` |
| frontmatter rules, wikilink grammar, the axes | `01-data-model.md` |
| auth, tenancy, token shape | `08-auth-and-tenancy.md` |

### Why this rule exists (audit, 2026-08-02)

The skill had drifted badly while nobody was looking:

- `09-local-iteration.md` asserted *"The workspace builds exactly **one**
  binary — the `escurel` CLI … There is **no `escurel serve` you can run
  locally today.**"* The workspace declares **eight** `[[bin]]` targets,
  one of which is `escurel-server`, and running it locally is now the
  ordinary dev loop. The reference even said *"(If this changes, this
  reference is the thing to update.)"* — it changed; it wasn't. Consumers
  were being sent to a throwaway-test-binary workaround for a problem
  that no longer existed.
- `04-consume-via-cli.md` was missing the `provenance`, `workflow` and
  `ui` subcommands, and `page delete`.
- `02-tool-surface.md`'s write-tools table omitted `delete_page`, a core
  write operation. (66 tools are exposed; the tables document 19. That
  curation is deliberate — but a *core write op* missing is drift, not
  curation.)

All three were corrected in the same PR that added this section. The
point of recording them is not the corrections — it is that **nothing
failed**. No test covers this. They were found only because someone read
the skill and compared it against a running system, which is exactly the
kind of check that does not happen on a schedule. Hence the obligation
sits at write time, in step 6b.

### Checking it cheaply

Not a substitute for reading it, but these catch the mechanical half:

```sh
# every docs/crates/examples path the skill cites still exists
grep -rhoE '`(docs|crates|examples|deploy|scripts)/[A-Za-z0-9_./-]+`' \
    .claude/skills/escurel-platform/ | tr -d '`' | sed 's/[.,)]$//' \
  | sort -u | while read -r p; do [ -e "$p" ] || echo "MISSING: $p"; done

# CLI subcommands the skill documents vs the ones that exist
./target/debug/escurel --help

# tools the server actually exposes
curl -s -X POST localhost:8080/mcp -H 'content-type: application/json' \
     -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

The same drift happens in the other direction — **code comments citing notes
that do not exist**. Both known cases were found only by sweeping for it: a
plan cited by eight files that had never left its branch, and a DuckDB
gotcha whose note `schema.rs` had pointed at since the migration was written.

```sh
# every docs/notes/ path cited from Rust source still exists
grep -rhoE '`?docs/notes/[A-Za-z0-9_./-]+\.md' --include=*.rs crates/ \
  | tr -d '`' | sort -u | while read -r p; do [ -e "$p" ] || echo "MISSING: $p"; done
```

## Disk discipline (build artifacts + worktrees)

This repo eats disk faster than any other in the fleet, and it does it
quietly. On 2026-08-02 the working copy had reached **845 GB** — 765 GB
of it `target/` dirs inside abandoned agent worktrees. Treat disk as a
resource you are responsible for, the same way you treat a green test
suite.

**Why it blows up.** The workspace compiled ~287 test/bin executables
(172 `tests/*.rs` files, each its own crate + binary — `escurel-server`
alone had 62; see the consolidation note at the end of this section for
where that stands now). Each is statically linked, so under the old default
`debug = 2` each embedded a full copy of the dependency graph's DWARF:
a sampled test binary was **704 MB, of which 81% was `.debug_*`**. One
built worktree ≈ 78 GB. The agent fan-out then multiplied that by the
number of worktrees. Nothing ever collected the garbage, and stale
hash-suffixed binaries from superseded builds accumulated forever.

**What's already done for you** (root `Cargo.toml`): `[profile.dev]`
sets `debug = "line-tables-only"` and `[profile.dev.package."*"]` sets
`debug = false`. Backtraces keep file:line in *our* code and keep
symbol names everywhere; only debugger-grade variable/type DWARF is
gone. If you genuinely need gdb/lldb, use `cargo test --profile
dev-debug`, then `cargo clean --profile dev-debug` when you're done.

**The rules.**

- **Remove a worktree when its branch merges.** This is PR-cycle step
  8. A merged worktree is pure waste — the commits are in `main`.
- **`target/` is never precious.** It is regenerable cache. If you are
  unsure whether to delete one, delete it. Never delete a *worktree*
  on that same instinct — check it's merged and clean first, which is
  exactly what the script's guards do.
- **Run `scripts/reclaim-disk.sh` at every natural pause** — after a
  merge, at the end of a multi-PR sequence, before starting a fan-out.
  With no flags it only reports:

  ```bash
  scripts/reclaim-disk.sh                 # report: sizes, ages, merged/unmerged
  scripts/reclaim-disk.sh --targets       # drop target/ untouched for 7d+
  scripts/reclaim-disk.sh --merged        # remove merged + clean worktrees
  scripts/reclaim-disk.sh --all --yes     # both, unattended
  ```

  `--merged` removes a worktree only when it is not the main worktree,
  is on a named branch, is clean, and is an ancestor of `origin/main`.
  Detached HEADs and dirty trees are reported and skipped.
- **Before a large agent fan-out**, set `CARGO_INCREMENTAL=0` in the
  agents' environment. Incremental state was 7.1 GB per worktree and
  is worthless in a worktree that gets built once and abandoned.
- **Don't** point every worktree at one shared `CARGO_TARGET_DIR`.
  Cargo takes an exclusive lock on the target dir, so parallel agents
  would serialise behind each other — it trades the disk problem for a
  throughput problem. Per-worktree targets plus real cleanup is the
  right shape; it also means `git worktree remove` frees the cache
  atomically.

**The two big crates are already consolidated.** `escurel-server` and
`escurel-index` no longer build one binary per test file: their tests
live in `tests/suite/` and are declared as modules from
`tests/suite/main.rs`, which Cargo builds as a single target named
`suite`. 120 linked executables became 4. **If you add a test file to
either crate, put it in `tests/suite/` and add its `mod` line** — a
file that is not listed there is silently never compiled, and nothing
warns you.

The layout matters: it must be `tests/suite/main.rs`, not
`tests/suite.rs`. A test target's root file resolves `mod x;` against
its *own* directory, so `tests/suite.rs` would look for `tests/x.rs`.

Two `escurel-server` files stay standalone on purpose —
`tests/logs_json.rs` and `tests/telemetry_filter.rs` both assert on the
process-global `tracing` subscriber and are mutually exclusive inside
one process. See the header of
`crates/escurel-server/tests/suite/main.rs`.

The remaining crates are deliberately untouched: the largest is
`escurel-runner` at 18 files, and below that the churn plus the lost
process isolation is not worth the link time saved.

See [`docs/notes/discovered/2026-08-02-cargo-target-disk-blowup.md`](docs/notes/discovered/2026-08-02-cargo-target-disk-blowup.md).

## Demo app + browser verification (rodney)

The demo/companion app is `apps/escurel-explore` (Flutter web). It
**tracks every backend capability** as it lands — when you add a tool
or surface to the server, wire it into escurel-explore in the same
sequence. The `escurel-server` binary can serve the built bundle at
`/` (set `ESCUREL_SERVE_DEMO_DIR=apps/escurel-explore/build/web`), so
the whole demo runs as one process alongside `/mcp`, `/ws`, `/metrics`.

Browser verification uses **[rodney](https://github.com/simonw/rodney)**
— a Go Chrome-automation CLI. Build it once
(`git clone https://github.com/simonw/rodney && cd rodney &&
go build -o ~/.local/bin/rodney .`; needs Go ≥ 1.21 + Chrome, both
present here as `google-chrome-stable` / `chromium`).

**Critical:** Flutter web renders to a CanvasKit `<canvas>` — there is
**no CSS-selectable DOM** for the app's widgets, so rodney's
`click "#id"` / `text "h1"` selector commands do **not** reach them.
Drive the app through Flutter's **semantics (accessibility) tree**
instead, via rodney's `ax-find --role <r> --name <label>` and
`ax-node` commands (proven working here: `ax-find --role button`
returns the node). For this to work the demo build force-enables
semantics at startup (`SemanticsBinding.instance.ensureSemantics()`)
and every interactive widget carries a **stable `Semantics(label: …)`**
— those labels are the selector contract; don't rename them casually.
The canonical end-to-end check is `scripts/verify-demo.sh` (builds the
web bundle, starts the gateway serving it, drives each panel with
rodney, exit-code gated).

## Reading order

If you are new to the codebase:

1. This file.
2. [`docs/README.md`](docs/README.md) for the spec reading order.
3. [`docs/spec/README.md`](docs/spec/README.md) for the architecture
   overview and locked decisions.
4. [`docs/notes/`](docs/notes/) for accumulated tribal knowledge.
