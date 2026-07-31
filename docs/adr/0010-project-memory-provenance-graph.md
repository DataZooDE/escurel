# ADR-0010 — Project-memory: the provenance-aware, expectation-aware graph

**Status:** Accepted, 2026-07-31.
**Builds on:** [ADR-0001](0001-duckdb-only-storage.md) (everything in the DuckDB
index is derivable from canonical markdown — the graph is a *derived* read
surface, never a new source of truth), [ADR-0006](0006-skill-packs.md) /
[ADR-0007](0007-pack-subscribe-import.md) (the ontology ships as a signed,
subscribable base-layer pack), and [ADR-0005](0005-page-layer-model.md) (the
entity-type skills land read-only base; tenants author instances in their
overlay).
**Scope:** the direction (project-memory as an *optional* vertical), the entity
model, the relation vocabulary, and the new bounded graph-query read surface.
The per-tool wire details live in [`../spec/protocol.md`](../spec/protocol.md);
the milestone plan lives in [`../spec/roadmap.md`](../spec/roadmap.md).

## Context

escurel is a knowledge base for agents: typed `skill`/`instance` markdown pages,
typed `[[skill::id]]` wikilinks, a derived DuckDB index, hybrid search, CRDT
live editing, and an event bus. The direction being ratified here is to make
escurel a **persistent project-memory system for data scientists/analysts**,
whose first-class entities are Stakeholder, Goal, Expectation, Constraint,
Hypothesis, Analysis, Result, and Decision, connected by a provenance-aware
graph.

The insight: a long-running project holds **two evolving networks** — a
*knowledge graph* (data → analysis → results → hypotheses) and an *expectation
graph* (stakeholder goals → priorities → constraints → success criteria) — and
most lost project context originates in the **expectation graph** (goals and
expectations that quietly changed), not the knowledge graph. The novel
contribution is therefore a **hypothesis-centric, expectation-aware memory**
that records *why* decisions were made, *why* paths were abandoned, and *how*
expectations drifted — for example, surfacing decisions that still rest on an
expectation that has since been superseded.

The load-bearing observation is that almost none of this needs a new storage
shape. escurel's existing primitives already carry the model:

- **Entities are `skill` pages.** A skill is a type declaration; an instance is
  a memory of that type. The eight entities are skills, not new `PageType`
  variants (the `skill`/`instance` enum stays closed).
- **Provenance relations are typed frontmatter wikilinks.** A frontmatter field
  `derived_from: [[analysis::x]]` already becomes a `links` row whose *kind* is
  recorded in `src_field` (`frontmatter.derived_from`) and whose target type is
  `link_skill`. No new column is needed to *record* a typed provenance edge.
- **Evolution is already recorded on the time axis** — event-typed skills
  (`at:`), supersession (`supersedes`), and `as_of` time-travel on
  `neighbours`/`expand`.
- **Distribution already exists** — the signed skill-pack mechanism
  (ADR-0006/0007) is the unit for shipping a curated ontology.

The one capability escurel lacks is a **bounded, multi-hop graph-query surface**
over the `links`/`pages` graph (today `neighbours` is one-hop only). That is the
only genuinely new engineering.

## Decision

1. **Project-memory is an optional vertical, shipped as the `project-memory`
   pack.** Eleven skill pages (Stakeholder, Goal, Expectation, Constraint,
   Priority, Success-Criterion, Hypothesis, Dataset, Analysis, Result, Decision)
   land read-only under `markdown/base/project-memory/`. The generic
   skill/instance KB contract is **unchanged and non-breaking**; the graph-query
   tools work for any tenant regardless of whether the pack is subscribed. This
   is a pack, not a rebrand: a tenant that never subscribes sees no change.

2. **The relation vocabulary is a fixed set of frontmatter fields** whose values
   are `[[skill::id]]` wikilinks; each becomes a `links` row (`src_field =
   frontmatter.<field>`, `link_skill = <target skill>`). The field name *is* the
   relation kind. Graph membership is definitional:
   - *Expectation graph:* `held_by`, `refines`, `prioritized_by`, `measured_by`,
     `constrains`, `supersedes` (on goal/expectation/constraint).
   - *Knowledge graph:* `uses`, `derived_from`, `produced_by`, `supports`,
     `refutes`, `prev_result`.
   - *Bridge:* `tests` (hypothesis→expectation), and
     `motivated_by`/`justified_by`/`addresses`/`abandons`/`decided_by`
     (decision→…). **Decision is the only entity with edges into both graphs**
     (`motivated_by` → expectation side, `justified_by` → knowledge side); that
     is what makes it the bridge and the primary "why" record. Provenance points
     backward in time, so a page names its causes at write time and never needs
     editing when downstream pages appear.

3. **The graph-query surface is derived, bounded, and read-only.** A single new
   schema object — the `resolved_links` VIEW — resolves the slug-valued
   `links.dst_page` to a real `dst_page_id` (INNER JOIN `links → pages` on
   `(slug, skill)`, so dangling links drop out) and projects the relation kind.
   New MCP read tools (`provenance_ancestry`, `provenance_path`,
   `abandoned_paths`, `expectation_drift`) run **parameterized, bounded**
   traversals over it. Consistent with the contract's "no direct SQL" rule, the
   agent never supplies SQL/Cypher text — only named scalars and an allow-listed
   relation filter; traversals are depth-bounded with a cycle guard. Results
   come back as page refs (one referent space). Every returned instance is
   ACL-filtered post-query (`may_read_instance`, fail-closed; skills public),
   and a multi-hop result drops any path through an unreadable node.

4. **The graph engine ships on stock DuckDB recursive CTEs; DuckPGQ is an
   optional, gated swap-in.** The tools sit behind a backend seam (`enum
   GraphBackend { RecursiveCte, DuckPgq }`) with identical input/output. The
   default and always-available backend is `WITH RECURSIVE` over
   `resolved_links` — zero new dependency, and it delivers the full query
   surface including the cross-graph `expectation_drift` (a pure two-join
   analytic). DuckPGQ (a community, unsigned extension) is a per-DuckDB-version
   build; whether a prebuilt binary exists for the pinned `libduckdb` is settled
   by an explicit go/no-go spike, and the DuckPGQ `MATCH` backend is added only
   if the spike is green. The extension risk therefore never blocks the feature.

5. **escurel stays automation-free (locked contract preserved).** escurel
   *serves* the provenance/drift/abandoned-path read queries — the raw
   materials. Synthesizing "the most promising next steps" is an **external
   agent's** job (the `escurel-demo-agent` pattern), fed by these queries. There
   is no server-side rules engine; state is recorded, not derived.

## Considered alternatives

- **(a) New first-class `PageType` variants per entity** (a `hypothesis` page
  type, a `decision` page type, …). Rejected: it forks the closed
  `skill`/`instance` model that every read/write path pattern-matches on, for no
  gain — a skill *is* the type-declaration mechanism, and modeling entities as
  skills keeps the entire existing tool surface (`resolve`/`expand`/`neighbours`/
  `search`/`list_instances`) working unchanged.

- **(b) A dedicated typed-edge table with a `relation_kind` column** (and an
  extended wikilink grammar to author it). Rejected as unnecessary: the
  relationship kind is already carried by `links.src_field`. A derived VIEW that
  projects `src_field` gives typed edges with no migration to the write path and
  no grammar change.

- **(c) DuckPGQ as a hard dependency, spike-and-block.** Rejected per review:
  community extensions track exact DuckDB builds, so making the whole feature
  wait on a binary that may not exist for the pinned version trades delivery risk
  for `MATCH` ergonomics we can add later. CTE-first with a gated swap-in gets
  100% of the value now.

- **(d) Reposition escurel entirely as project-memory (default identity).**
  Rejected for v1 of this direction: an optional pack is non-breaking and keeps
  the generic KB tenants working untouched. Repositioning can follow once the
  vertical proves out.

## Consequences

- **What does NOT change:** the `skill`/`instance` `PageType` enum; the wikilink
  grammar and index-time validation; the twelve-tool agent contract; the pack
  import/layer/shadow model; `pages`/`links`/`blocks` table shapes;
  audit-and-rebuild (the VIEW and any property graph are catalog objects
  rebuilt on every open, fully derivable from markdown per ADR-0001).

- **What is added:** one VIEW (`resolved_links`) and its `ensure_` lifecycle;
  four bounded read tools with their type/client/CLI surface and the
  `cli_parity` ratchet rows; the `project-memory` pack fixtures + companion doc;
  optionally the DuckPGQ backend arm behind the spike.

- **Doc drift to reconcile as the work lands:** `roadmap.md` gains a post-v1
  milestone; `protocol.md` / `contract/agent-interface.md` gain the read tools;
  `README.md` notes the optional vertical.

- **Tests:** each increment merges on a no-mock integration test — the pack
  round-trips through real `import_pack` + `neighbours`; the query tools author
  real provenance chains through `update_page` and assert ancestry depth,
  drift detection, dangling-link exclusion, and ACL fail-closed behaviour
  against a live gateway.
