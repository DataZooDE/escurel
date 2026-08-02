# Cargo target dirs reached 845 GB — debuginfo × 287 binaries × N worktrees

**Date:** 2026-08-02

## Symptom

The escurel working copy occupied **845 GB**, over half of a 1.4 TB
`~/Projects/datazoo` tree. `du` located it precisely:

```
765G  .claude/worktrees      # 12 agent worktrees, each with its own target/
 79G  target                 # the main worktree's own build cache
```

Individual agent worktrees held 161 GB, 158 GB, 148 GB, 97 GB, 95 GB of
`target/` — all last written 2026-07-14..18, i.e. cold for two weeks
while their branches sat unmerged.

## Root cause

Three multipliers stacked:

1. **Debuginfo dominates each binary.** The dev profile had no explicit
   `debug` setting, so it defaulted to `debug = 2` (full DWARF).
   Measured on `target/debug/deps/e2e-*`:

   ```
   704 MB  as built
   135 MB  after `strip --strip-debug`
   ```

   **81% of every binary was `.debug_*` sections.**

2. **The workspace links ~287 executables.** Cargo compiles every
   `tests/*.rs` as its own crate and links its own binary; there are 172
   such files (`escurel-server` 62, `escurel-index` 47, `escurel-runner`
   17, …), plus unit-test and bin targets. Because each links
   statically, the *same* dependency DWARF (duckdb, arrow, tokio,
   kreuzberg) was duplicated ~287 times inside a single `target/`.
   `target/debug/deps` alone was 136 GB of the 144 GB.

3. **One target/ per worktree, never collected.** The agent fan-out
   creates a worktree per task. Nothing removed them after their PRs,
   and nothing removed the build cache of worktrees still open. Stale
   hash-suffixed binaries from superseded builds also accumulated
   indefinitely (`ws_session-e788873…` and `ws_session-e1157315…` both
   present, ~730 MB each).

Independent evidence of the rate: during a single ~30-minute session,
another agent working in this repo grew `target/` from 79 GB to 144 GB.

## Fix

Root `Cargo.toml`:

```toml
[profile.dev]
debug = "line-tables-only"

[profile.dev.package."*"]
debug = false
```

`line-tables-only` retains the file:line mapping a test-failure
backtrace needs; `package."*"` drops debuginfo for dependencies only
(workspace members keep their line tables). Symbol *names* live in the
symtab, not in `.debug_*`, so dep frames still show function names in
backtraces — they just lose file:line. A `dev-debug` profile
(`inherits = "dev"`, `debug = 2`) is the opt-in escape hatch for an
actual gdb/lldb session; it builds into `target/dev-debug/` so it costs
disk only while in use.

Plus `scripts/reclaim-disk.sh` for the recurring collection, and a
*Disk discipline* section in `CLAUDE.md` making worktree removal step 8
of the PR cycle.

## How to recognise it next time

- `du -sh target/debug/deps` is a large fraction of the repo.
- `find target/debug/deps -maxdepth 1 -type f -executable | wc -l`
  returns hundreds, and the binaries are ~700 MB each.
- The gap between a binary's size and its `strip --strip-debug` size is
  the debuginfo you're paying for, once per binary.

## Gotchas

- **Changing a profile invalidates the whole cache.** The first build
  after this lands is a full rebuild. Don't land it underneath someone
  mid-session.
- **Don't share one `CARGO_TARGET_DIR` across worktrees** to dedup.
  Cargo takes an exclusive lock on the target dir; parallel agents
  would serialise. Per-worktree targets + real cleanup keeps the
  parallelism and makes `git worktree remove` free the cache atomically.
- **`target/` is regenerable; worktrees are not.** All 17 worktrees
  here held unmerged commits (1–7 ahead of `origin/main`). Deleting
  their `target/` was free; deleting the worktrees would have lost
  work. `reclaim-disk.sh --merged` guards on merged + clean + named
  branch for exactly this reason.
