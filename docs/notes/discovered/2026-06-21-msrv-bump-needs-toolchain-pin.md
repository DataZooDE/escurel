# Bumping the workspace MSRV must also bump `rust-toolchain.toml`

**Symptom.** After raising the workspace `rust-version` to `1.91` (for
kreuzberg 4.9.9, commit a06ad97), the local four-check gate stayed green but
the re-enabled GitHub CI job (`fmt + clippy + test + build`) failed at
`cargo clippy` with a wall of:

```
escurel-server@1.0.0 requires rustc 1.91
escurel-storage@1.0.0 requires rustc 1.91
...
##[error]Process completed with exit code 101.
```

**Cause.** Two places encode the toolchain and they drifted apart:

- `Cargo.toml` → `[workspace.package] rust-version = "1.91"` (the *declared
  MSRV*; every crate inherits it). Bumped in a06ad97.
- `rust-toolchain.toml` → `channel = "1.88.0"` (the *toolchain CI installs*).
  **Not** bumped — still 1.88.0.

CI honours `rust-toolchain.toml`, so the runner installed 1.88.0 and cargo
refused to build crates declaring `rust-version = 1.91`. Locally this was
masked: the dev box has a system rustc (Arch `1.96.0`) and **no rustup**, so
`rust-toolchain.toml` is ignored entirely — every local build silently used
1.96, comfortably above the MSRV.

**Fix.** Pin `rust-toolchain.toml` to the MSRV: `channel = "1.91.0"`. The
established convention is *toolchain pin == declared MSRV* (it was 1.88.0
when the MSRV was 1.88), so CI both builds and enforces the floor.

**Recognise it next time.** Any change to `[workspace.package] rust-version`
must be made in lockstep with `rust-toolchain.toml`'s `channel`. If a CI
clippy/build step dies with `requires rustc <X>` while the local gate is
green, suspect this drift first — especially on a box without rustup, where
the toolchain file is a no-op and can't catch it locally. To actually
exercise the pinned MSRV locally you need rustup (`rustup toolchain install
1.91.0`); the bare system rustc won't honour the file.
