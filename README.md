# Escurel

> *Escurel* — old French for *knowledge-base*.

Escurel is a multi-tenant knowledge-base service for agents. It exposes
a 12-tool MCP surface (plus HTTP and WebSocket bindings), stores
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

## CLI & TUI

The `escurel` binary (crate `escurel-cli`) is a gh/aws-style client
over the gateway: one resource noun, one verb, one RPC. It speaks the
HTTP MCP endpoint (`--server` / `ESCUREL_SERVER`, default
`http://127.0.0.1:8080`) with an OIDC bearer (`--token` /
`ESCUREL_TOKEN`).

```sh
escurel skill list                        # Tier-1 skill catalogue
escurel instance list --skill customer    # instances of a skill
escurel page expand markdown/instances/customer/acme.md
escurel link neighbours <page_id> --direction in
escurel search "renewal" --k 5
escurel resolve '[[customer::acme]]'
escurel event capture --title "Renewal call" --body "…"
escurel event inbox                       # unprocessed events
escurel event assign --event <id> --instance <page_id>
escurel query run <query_id> --params '{"skill":"customer"}'
escurel chat append -g <group> --content "hi"
escurel admin tenant list                 # operator surface
```

Every command emits stable JSON by default; pass `--format table` for
a human-readable view. Errors are emitted as JSON on stderr with a
non-zero exit, so an agent can branch on them.

`escurel ui` launches an interactive **k9s-style terminal browser**
(crate `escurel-tui`) against the same `--server` / `--token`: drill
skills → instances → entity, inspect outgoing links + backlinks,
browse the event inbox and per-instance history, filter with `/`, `?`
for help, `q` to quit.

## License

Source-available under the [Business Source License 1.1](LICENSE),
converting to MPL 2.0 five years after first publication. Production
use is permitted; offering Escurel to third parties on a hosted or
embedded basis requires a commercial license.

## Contact

Maintained by [DataZoo GmbH](https://data-zoo.de). Open an issue for
spec ambiguities or implementation questions.
