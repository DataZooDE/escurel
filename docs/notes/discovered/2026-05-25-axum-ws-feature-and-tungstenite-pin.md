# axum 0.8 `ws` feature is opt-in; tungstenite 0.24 `Message::Text` takes `String`

**Date:** 2026-05-25
**Scope:** `crates/escurel-server` (M3.6 WebSocket scaffolding)

## Symptom 1 — silent missing extractor

Without `axum = { version = "0.8", features = ["ws"] }` the
`use axum::extract::ws::{WebSocket, WebSocketUpgrade, Message};`
import fails with "no `ws` in module `extract`". Easy to miss
because axum 0.7 had `ws` enabled by default; we bumped to 0.8 in
an earlier PR and the default-feature set tightened in 0.8.

**Fix.** Add `features = ["ws"]` to the axum dependency in
`crates/escurel-server/Cargo.toml`.

## Symptom 2 — clippy `useless_conversion` on `Message::Text`

The natural-looking
`sock.send(Message::Text(value.to_string().into()))` works on
axum's reexport (axum's `Message::Text` wraps a `Utf8Bytes`) but
fails clippy on `tokio-tungstenite` 0.24, whose
`tungstenite::protocol::Message::Text` variant takes a plain
`String`. The `.into()` is a no-op `String → String` conversion
and `-D warnings` rejects it.

**Fix.** Drop the `.into()` when calling tungstenite directly.
Symmetrically, `Message::Text(t)` in tungstenite 0.24 gives back
a `String`, so don't `t.to_string()` it — use `t` directly.

`tokio-tungstenite` 0.26+ migrated to `Utf8Bytes` to match axum,
but the workspace currently resolves to 0.24 via wiremock's
transitive constraints. When we eventually bump tungstenite past
0.26, expect the opposite lint to fire and re-add the `.into()`.

## How to recognise next time

- Symptom: an axum WS handler compiles but tungstenite-client
  test code fails clippy on `useless_conversion` or "expected
  `String`, found `Utf8Bytes`".
- Smell: mixing axum's `Message` reexport with a `tokio-tungstenite`
  test client in the same crate. They are *separate* types from
  the same upstream crate at potentially different versions.
- Quick fix: keep the axum-side handler using the axum reexport
  shape; keep the tungstenite client test using its native
  `Message::Text(String)` shape. Don't cross-import.
