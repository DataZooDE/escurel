# Cargo refuses to operate on a workspace with empty members

**Date:** 2026-05-24
**Scope:** repo bootstrap, workspace setup

## Symptom

A `Cargo.toml` containing only

```toml
[workspace]
resolver = "2"
members = []
```

succeeds for `cargo metadata` but every other command fails with:

```
error: manifest path `/path/to/repo` contains no package:
The manifest is virtual, and the workspace has no members.
```

This affects `cargo fmt --all -- --check`, `cargo clippy --workspace`,
`cargo test --workspace`, `cargo build --workspace`.

Reproduced with cargo 1.95.0 on Arch.

## Cause

Cargo's virtual-manifest mode requires at least one member to be
meaningful. The error message is accurate: there is no package to
operate on. This is not a bug, but it does block the "ship a workspace
shell first, add crates incrementally" pattern that looked clean on
paper.

## Fix

Defer materialising `Cargo.toml` (and `rust-toolchain.toml`, and the
cargo steps in CI) until the PR that introduces the first crate. The
two land together. Until then the repo is a docs-and-scaffolding repo
from cargo's perspective; nothing breaks.

The CI workflow in PR 0 verifies the non-Rust scaffolding files exist
and that GitHub Actions is wired up correctly. PR 1 expands the
workflow with the real cargo stages (`fmt`, `clippy`, `test`,
`build`).

## How to recognise next time

If you see `contains no package: The manifest is virtual, and the
workspace has no members` and your `Cargo.toml` is intentionally
empty: you're trying to use cargo before you have anything for it
to do. Either add a member or defer the `Cargo.toml` until you do.

## Related plan revision

`/home/jr/.claude/plans/go-over-the-specs-elegant-yao.md` PR 0
originally listed `Cargo.toml` + `rust-toolchain.toml` + full cargo
CI stages. Those moved to PR 1. The plan was edited to reflect this
when the discovery was made.
