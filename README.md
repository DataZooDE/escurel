# Escurel

> *Escurel* — old French for *knowledge-base*.

Escurel is a multi-tenant knowledge-base service for agents. It exposes
a 12-tool MCP surface (plus HTTP, WebSocket and gRPC bindings), stores
each tenant's data in a single per-tenant DuckDB file using the `vss`
and `fts` extensions, and treats a `pages/` markdown directory as the
canonical source of truth. Live multi-author editing is backed by a
Loro CRDT layer persisted into DuckDB.

## Status

**v1 specification — no implementation yet.** The spec is settled and
the architecture is locked. The Rust implementation will land in this
repo alongside the spec.

## Read the spec

- Start at [`docs/README.md`](docs/README.md) for the reading order.
- The agent ↔ KB contract lives in
  [`docs/contract/agent-interface.md`](docs/contract/agent-interface.md).
- The implementation spec is under [`docs/spec/`](docs/spec/) (storage,
  protocol, platform, roadmap).
- The single load-bearing architectural decision is captured as an ADR
  in [`docs/adr/0001-duckdb-only-storage.md`](docs/adr/0001-duckdb-only-storage.md).
- Deployment binding to the DataZoo Hetzner substrate is in
  [`docs/deploy/`](docs/deploy/).

## License

Source-available under the [Business Source License 1.1](LICENSE),
converting to MPL 2.0 five years after first publication. Production
use is permitted; offering Escurel to third parties on a hosted or
embedded basis requires a commercial license.

## Contact

Maintained by [DataZoo GmbH](https://data-zoo.de). Open an issue for
spec ambiguities or implementation questions.
