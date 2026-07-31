# project-memory — a data-science project-memory pack

This directory is the source of the **`project-memory`** skill pack
(vertical `data-science`): a curated ontology that turns an escurel
tenant into a persistent, provenance-aware memory for a long-running
data-science / analytics project. It ratifies
[ADR-0010](../../docs/adr/0010-project-memory-provenance-graph.md).

It is an **optional** pack. A tenant that never subscribes to it is a
plain escurel knowledge base; subscribing adds the fourteen entity skills
below as a read-only base layer, and the tenant authors its own
instances in its overlay.

## The two graphs

A project holds two evolving networks, and most lost context comes from
the second:

- **Knowledge graph** — `dataset → analysis → result → hypothesis`.
- **Expectation graph** — `stakeholder → goal → expectation/constraint
  → success_criterion`, ranked by `priority`.

`decision` is the **bridge**: `motivated_by` reaches into the
expectation graph (why), `justified_by` into the knowledge graph
(evidence).

## Layout

- `skills/` — the fourteen type declarations shipped in the pack
  (`project-memory` is the documentation/overview skill; the other
  thirteen are the first-class entities, including `project` /
  `conclusion` for sub-project containment + closing).
- `instances/` — a worked example (a customer-churn project) used by the
  integration tests and as a demonstration. Real tenants author their
  own instances; these ship only as a sample.

## The worked example

A churn project where the **expectation drifts**: the team first expects
"one model for the whole base" (`expectation::churn-v1`), builds a GBM,
gets AUC 0.82, and **decides** to ship it. The expectation is then
reframed to "high-value churn only" (`expectation::churn-v2`, which
`supersedes` v1) — which **orphans** the earlier ship decision. That
orphaning is exactly the lost-context signal the memory is built to
surface (`expectation_drift`).

## Relation vocabulary

See [`skills/project-memory.md`](skills/project-memory.md) for the full
list of relation fields and which graph each belongs to.
