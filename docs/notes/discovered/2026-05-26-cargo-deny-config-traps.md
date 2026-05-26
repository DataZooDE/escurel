# `cargo-deny` config traps wiring up the license/advisory gate

**Date:** 2026-05-26
**Scope:** M5 license audit + dep freeze (`deny.toml`)

Setting up `cargo deny check` for the workspace surfaced three
non-obvious behaviours. cargo-deny 0.19.7 was the version in use.

## 1. `allow-wildcard-paths` silently does nothing without `publish = false`

Intra-workspace path deps (`escurel-index = { path = "../escurel-index" }`)
carry no version requirement, so `[bans] wildcards = "deny"` flags every
one of them as a wildcard dependency:

```
error[wildcard]: found 4 wildcard dependencies for crate 'escurel-index'.
allow-wildcard-paths is enabled, but does not apply to public crates as
crates.io disallows path dependencies.
```

`allow-wildcard-paths = true` is the documented fix — but it only applies
to crates marked `publish = false`. The escurel members inherit
`version = "0.0.0"` and do **not** set `publish = false`, so cargo-deny
treats them as publishable and refuses to exempt their path deps. The
error message says exactly this once you read past the first clause.

**Recognise it:** every wildcard error points at a `{ path = "../..." }`
line in one of your own crates, and the error text mentions "does not
apply to public crates."

**Fix:** mark each workspace member `publish = false`. Note that
`publish = false` in the root `[workspace.package]` is **not** inherited
automatically — each member must opt in (`publish.workspace = true`) or
set it directly. Until that lands, keep `wildcards = "warn"` (there were
zero *external* wildcards in the tree, so nothing real is lost).

## 2. `[graph] all-features = true` resurrects unused-feature advisories

The license audit wants `all-features = true` so a license can't hide
behind an optional feature. But the same flag activates feature paths the
shipped binary never compiles. `aws-smithy-http-client` has an optional
legacy `rustls 0.21` path; under `all-features` that pulls in
`rustls-webpki 0.101.7` and fires three RUSTSEC advisories
(2026-0098/0099/0104). With default features the path is absent —
`cargo tree -i rustls@0.21.12` returns "did not match any packages." The
real binary uses the patched `rustls 0.23` / `webpki 0.103.13`.

**Recognise it:** an advisory's dependency chain runs through an
optional-feature-only crate, and `cargo tree -i <crate>` (no
`--all-features`) cannot find it.

**Fix:** there's no per-check feature scope in cargo-deny, so ignore the
advisory with a dated rationale that records the `cargo tree -i` evidence.
Re-verify at each dep freeze.

## 3. `exceptions`/`clarify` for a crate that no longer needs them is a hard error

An early `deny.toml` carried a `ring` license clarify + an `OpenSSL`
exception (true of old `ring`). `ring 0.17.14` now declares
`Apache-2.0 AND ISC` cleanly, so the exception is unmatched:

```
error: unmatched license exception
```

An *unused* exception fails the check just like a missing allow does.

**Recognise it:** the error points at a line in `deny.toml` itself, not at
a crate's `Cargo.toml`. Same for an `allow`ed license that's never
encountered (that one is only a warning: `license-not-encountered`).

**Fix:** delete the stale exception/clarify; keep `allow`/`exceptions`
minimal and matched to the tree you actually have.
