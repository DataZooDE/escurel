# tonic 0.12 sync interceptor + async verifier = runtime deadlock

**Date.** 2026-05-25.
**Where.** `crates/escurel-server/src/grpc.rs` (M3.5b).

## Symptom

`cargo test -p escurel-server --test grpc_read_tools` hangs
forever on the first call that goes through an
`EscurelServer::with_interceptor(...)` interceptor doing OIDC
verification. The test process never produces a "test result"
line; `ps` shows the test binary parked.

## What caused it

Tonic 0.12's `Interceptor` is a synchronous trait:

```rust
fn call(&mut self, req: Request<()>) -> Result<Request<()>, Status>
```

So the natural way to plug an async `OidcVerifier::verify` into it
is `futures::executor::block_on(verifier.verify(token))`. That
works in isolation but deadlocks in tonic's request path:

1. The tonic server runs the interceptor on a tokio worker thread
   inside the handler chain.
2. `block_on` parks **that** worker thread until `verify` resolves.
3. `verify` issues an async JWKS fetch via reqwest, which needs a
   tokio worker to poll the I/O future.
4. The runtime has no spare worker (single-call test) or even if
   it does, the executor used by `futures::executor::block_on` is
   a foreign executor, not the tokio runtime — the reqwest future
   never makes progress.

The connection never replies; the client's `await` never returns;
`cargo test` hangs.

`tokio::task::block_in_place` + `Handle::current().block_on()` is
the standard escape hatch when you must bridge sync→async on a
tokio worker. But that complication is unnecessary because tonic
handlers are *already* `async fn` — you can do the auth `.await`
inside the handler directly and skip the interceptor entirely.

## The fix

Move auth + quota enforcement out of the tonic interceptor and
into the (already-async) gRPC handlers via a single shared helper:

```rust
impl EscurelGrpc {
    async fn enforce<R>(
        &self,
        req: &Request<R>,
        dim: Option<Dimension>,
    ) -> Result<Option<AuthContext>, Status> { ... }
}

#[tonic::async_trait]
impl Escurel for EscurelGrpc {
    async fn list_skills(&self, req: Request<…>) -> Result<…> {
        self.enforce(&req, Some(Dimension::Queries)).await?;
        // …
    }
}
```

`enforce` does both the `verifier.verify(...).await` and the
`quota.try_consume(...)` call, returning `Status::unauthenticated`
or `Status::resource_exhausted` on failure. The auth path stays on
the tokio runtime that's already running.

The gRPC tests after the fix:
`cargo test -p escurel-server --test grpc_read_tools` — 7/7 pass
in ~5s.

## How to recognise next time

- Symptom: a test that uses a tonic interceptor (or Tower
  middleware) hangs with no output, both client and server
  parked.
- Smell: `block_on` (any flavour) inside a tonic 0.12 interceptor
  closure.
- Quick check: replace the interceptor with a no-op identity
  interceptor — does the hang go away? If so, the interceptor's
  sync→async bridge is the culprit.
- Quick fix: move the async work into the (already-async)
  handler. If you genuinely need a Tower middleware (e.g. for
  uniform logging across services), implement it as a
  `tower::Layer` + `Service` pair — those are `async`-native and
  don't require the bridge.

If tonic 0.13+ ships an async-native `Interceptor`, revisit and
inline auth as a Layer for symmetry with the HTTP gateway.
