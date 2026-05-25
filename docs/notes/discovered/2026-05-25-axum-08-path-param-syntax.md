# axum 0.7 → 0.8 path-param syntax migration is silent at compile time

**Date.** 2026-05-25.
**Where.** `examples/echo-app/src/lib.rs` (M-DX-4).

## Symptom

The echo-app handler `GET /pages/{slug}` returned `404` for every
slug — including ones we had just confirmed resolved via a direct
`Client::resolve` probe one line earlier. No error, no panic, no
log message: the router quietly never matched the route at all,
and axum fell through to its built-in 404.

## What caused it

axum 0.7 expected path params spelt `:slug`. axum 0.8 switched to
`{slug}` (and accepts only the new form). The echo-app crate
declared `axum = "0.7"` but wrote `.route("/pages/{slug}", …)` —
i.e. 0.8 syntax against a 0.7 dependency. axum 0.7 happily
**registers** the route as a literal-path match for the
seven-character string `{slug}` and matches no actual incoming
request.

`escurel-server` already pulls axum 0.8 in the same workspace, so
the workspace's resolver picks 0.8 for the shared crate but each
crate uses its own pinned version. echo-app at `^0.7` got 0.7; the
syntax mismatch produced silent 404s.

## The fix

Pin `axum = "0.8"` in `examples/echo-app/Cargo.toml`. After the
bump, `cargo test -p echo-app --test e2e` flipped from `404` to
`200` immediately.

## How to recognise next time

- Symptom: an axum handler that obviously *should* match a route
  returns 404, the handler body never runs, no `tracing`
  middleware sees the request.
- Quick check: in axum 0.8 a literal `{slug}` in a `.route(...)`
  string is a path param. In axum 0.7 the same string is a literal
  brace-slug-brace path component. If the dependency tree mixes
  versions, the syntax that works in one crate silently fails in
  another.
- Quick fix: `cargo tree -p <crate> | grep axum` to confirm the
  resolved version, then pick the syntax that matches.

The lesson generalises beyond axum — any router crate that
changes path-param syntax across a major bump (warp, actix,
matchit) hits the same trap when a workspace mixes versions.
