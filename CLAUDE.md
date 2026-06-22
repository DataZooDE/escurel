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

   **CI policy.** GitHub Actions CI is **paused** (`workflow_dispatch`
   only) during bootstrap. Local `cargo fmt --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace --all-targets`, and `cargo build
   --workspace --release` must all pass before merge. The trade-off:
   we skip the 20–30 min cold duckdb compile per PR in exchange for
   trusting the local toolchain. Before declaring v1 stable, uncomment
   the `pull_request` + `push` triggers in `.github/workflows/ci.yml`
   so the safety net is back on for any post-v1 work.

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
7. Merge with `gh pr merge --squash --delete-branch`. (CI is
   paused during bootstrap — see principle 2.)

## Locked decisions (current bootstrap)

- **PR workflow:** feature branch → GitHub PR against `main` →
  local checks green → squash-merge. GitHub Actions CI is paused
  during bootstrap (see principle 2).
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
