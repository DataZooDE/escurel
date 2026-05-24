# examples/

Top-level home for tenants that document escurel by example. Each
subdirectory is a self-contained tenant — `skills/` declaring page
types, `instances/` carrying memories — that escurel parses, indexes,
and serves like any other tenant.

Use these to:

- Drive integration tests against real markdown (no fixtures derived
  from synthetic shapes).
- Seed the `escurel-explore` editor's fixture mode so the UI ships
  with a working corpus.
- Reach for as starting points when modelling a new domain — copy a
  tenant, rename, edit.

## Tenants

- [`crm-demo/`](crm-demo/) — a five-instance sales-lifecycle chain
  (engagement → lead → opportunity → project) plus the supporting
  customer and contact skills. Companion to the
  [skill-based CRM UX brief](../docs/notes/) — see scenario A
  (Hoffmann) for the chain modelled here.
