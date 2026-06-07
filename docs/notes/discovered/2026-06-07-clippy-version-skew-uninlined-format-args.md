# Local clippy is newer than CI's — `uninlined_format_args` skew

**Symptom.** The full local gate was green
(`cargo clippy --workspace --all-targets -- -D warnings` passed), but CI
failed on `cargo clippy` with:

```
error: variables can be used directly in the `format!` string
  --> crates/escurel-runner-core/src/reconciler.rs:337
  = note: `-D clippy::uninlined-format-args` implied by `-D warnings`
```

on `format!("sha256:{:x}", digest)`.

**Cause.** CI pins the toolchain via `rust-toolchain.toml`
(`channel = "1.88.0"`); the local Arch box runs a *newer* system
toolchain (`rustc 1.96.0`, `clippy 0.1.96`, no rustup). Clippy's
`uninlined_format_args` changed across versions: **1.88 lints positional
args that carry a format spec** (`{:x}`, `{:?}`, …) when the arg is a
simple identifier, suggesting `{digest:x}`; **1.96 no longer lints the
spec'd-arg case** (only the plain `{}` case). So a spec'd positional arg
passes locally on 1.96 and fails on CI's 1.88. Plain `{}` uninlined args
are caught by both, so they don't produce this surprise.

**Fix.** Inline the arg: `format!("sha256:{digest:x}")`.

**How to recognise / avoid next time.**
- The merge gate is CI's pinned toolchain (`rust-toolchain.toml`), not
  whatever the local box happens to have. When local clippy is green but
  you can't run the pinned channel (no rustup here), audit new
  format-family macros for **positional args that are bare identifiers**
  — both plain `{}` and spec'd `{:x}`/`{:?}` — and inline them
  (`{ident}` / `{ident:x}`). Method-call/expression args
  (`format!("{:09}", x.subsec_nanos())`) are *not* inlinable and are
  safe.
- Quick sweep:
  `grep -nE '(format!|write!|writeln!|eprintln!|println!|panic!|info!|warn!|error!|debug!|trace!)\("[^"]*\{[^}]*\}[^"]*",\s*[a-z_][a-z0-9_]*\s*[,)]' <files>`
  finds positional bare-identifier args across the changed files.
- Better long-term: install rustup so the pinned 1.88 toolchain can be
  run locally before pushing.
