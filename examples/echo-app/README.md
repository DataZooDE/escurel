# echo-app

Minimal demonstration backend for the contract in
[`docs/spec/dx.md`](../../docs/spec/dx.md) §"Chaining recipe".

Real shape:

```
escurel  →  echo-app  →  HTTP client
```

`echo-app` is a tiny axum service that, on `GET /pages/{slug}`,
resolves `[[customer::{slug}]]` against escurel, expands the
matching page, and returns the markdown body. It exists to prove
end-to-end that the `escurel-client` + `escurel-test-support` pair
lets a downstream application stand up the full chain in one
integration-test file with no mocks at the boundary.

The acceptance test is
[`tests/e2e.rs`](tests/e2e.rs) — `dashboard_round_trips_through_echo_app`.

## Why this shape

The dx.md recipe is `escurel → app-backend → triton → app-frontend`,
but triton is not a sibling repo in this workspace. The echo-app
substitutes for the application's HTTP edge so the test still
reads like the spec's snippet.
