---
type: skill
id: project-memory
description: How this project's memory is organised — the two-graph model (knowledge + expectation) and the provenance relation vocabulary. Read this first when entering a data-science project tenant.
required_frontmatter: []
optional_frontmatter: []
---

# project-memory — how this project's memory is organised

This tenant is a **persistent project-memory** for a data-science /
analytics project. On top of escurel's generic skill/instance model it
ships a small ontology whose instances form **two evolving graphs**:

- **Knowledge graph** — how understanding is built:
  `dataset → analysis → result → hypothesis`.
- **Expectation graph** — what stakeholders want and how it changes:
  `stakeholder → goal → expectation / constraint → success_criterion`,
  with `priority` ranking goals.

Most lost project context comes from the **expectation graph** — a goal
or expectation that quietly changed — not from the data. So the memory
is *hypothesis-centric* and *expectation-aware*: it records not just
what was found, but **why** decisions were made, **why** paths were
abandoned, and **how** expectations drifted.

## Entities (skills)

| entity | graph | event-typed | what it captures |
|---|---|---|---|
| `stakeholder` | expectation | no | a person/role whose wants shape the project |
| `goal` | expectation | yes | a desired outcome held by a stakeholder |
| `expectation` | expectation | yes | a concrete, revisable statement of what's expected |
| `constraint` | expectation | yes | a limit the project must respect |
| `priority` | expectation | no | a named ranking level for goals |
| `success_criterion` | expectation | no | a measurable bar deciding whether a goal is met |
| `hypothesis` | knowledge/bridge | yes | a falsifiable claim linking expectation to data |
| `dataset` | knowledge | no | a data source an analysis consumes |
| `analysis` | knowledge | yes | a unit of analytical work over datasets |
| `result` | knowledge | yes | a measured finding produced by an analysis |
| `decision` | **bridge** | yes | a committed choice — the primary "why" record |

## Relation vocabulary (typed provenance edges)

Relations are frontmatter fields whose values are `[[skill::id]]`
wikilinks. Each becomes a typed edge; the **field name is the relation
kind**. Provenance points backward in time — a page names its causes at
write time.

- **Expectation graph:** `held_by`, `refines`, `prioritized_by`,
  `measured_by`, `constrains`, `supersedes`.
- **Knowledge graph:** `uses`, `derived_from`, `produced_by`,
  `supports`, `refutes`, `prev_result`.
- **Bridge:** `tests` (hypothesis→expectation), and a decision's
  `motivated_by` (→ expectation side), `justified_by` (→ knowledge
  side), `addresses`, `abandons`, `decided_by`.

`decision` is the only entity with edges into **both** graphs — that is
what makes it the bridge between "why we wanted this" and "what the data
said."

## How to read the memory

- **Why was this decided?** `neighbours(<decision>, out)` — the
  `motivated_by`/`justified_by`/`addresses` targets.
- **Why was a path abandoned?** a `hypothesis`/`analysis`/`decision`
  with `status: abandoned|orphaned` and an `abandoned_because:` note,
  plus an inbound `abandons` edge from the superseding decision.
- **How did expectations evolve?** the `supersedes` chain over
  `expectation` instances, ordered by `at`.
- **What rests on a stale expectation?** a decision whose `motivated_by`
  expectation was later superseded — surfaced by `expectation_drift`.

This page is documentation only; it declares no instances.
