# Agent ↔ KB interaction design

**Date:** 2026-05-17.
**Status:** Proposal. Open questions at the bottom.
**Scope:** The contract between an agent (or agent harness) and a
KB tenant. Defines the tool surface exposed over MCP and CLI, the
meta-skill that teaches the agent how to use the surface, and the
behavioural promises both sides depend on.

This document is a design proposal, not a spec. It draws on what
worked in the real-LLM run (`real-llm-run/results.md`) and on the
three-axis reframing in the paper.

## Design principles

The design follows from the paper:

1. **One referent space.** The agent has exactly one mental model:
   "find the typed page or block I need." No second model for
   external data, no second model for time. Same primitives across
   the kind / time / origin axes.

2. **Stateless per call.** Each tool call returns a complete result.
   No cursors, no session affinity, no pinned context. The agent
   keeps its own working set; the KB just answers questions.

3. **Tier-1 cheap, Tier-2 lazy.** Discovery primitives return
   metadata first (skill id + description + frontmatter
   excerpt). Body retrieval requires a deliberate `expand()`.
   Matches the simulated-and-now-real-LLM cost result: 1 to 2
   tool calls per cold start.

4. **Read OR write, never both in one call.** All read primitives
   are safe to call speculatively. All write primitives go through
   validation. No tool both retrieves and mutates.

5. **Validation is a first-class output.** Both `validate()` (dry
   run) and `update_page()` (live) return the same issue list. The
   authoring feedback channel is the engine path — the same one
   verified in H-AUTHORING-1.

6. **The same surface exposes markdown and external data.** A
   `[[table::*]]` instance whose body declares
   `catalog: ducklake` is reached via the same `resolve()` /
   `expand()` calls as a `[[person::*]]`. The dispatcher hides
   the lane; the agent does not need to know which store backs
   which instance.

## The tool surface

### Agent-facing (exposed over MCP)

Twelve tools, grouped by axis. The four write tools split between
"live" (CRDT op stream) and "fallback" (whole-page submission) — see
§"Write path in detail" below.

| tool | inputs | output | axis | notes |
|---|---|---|---|---|
| `search` | `q: str`, `k: int = 10`, `granularity: 'block'\|'page' = 'block'`, `page_type?: 'skill'\|'instance'\|'any' = 'any'`, `skill?: str` | list of hits (shape depends on granularity — see §"`search` granularity in detail") | kind | natural-language vector + FTS hybrid; optional `skill=` filter pushes a `link_skill` predicate to DuckDB |
| `resolve` | `wikilink: str` | `{page_id, skill, page_type, description, exists: bool, error?}` | kind | parses `[[skill::id#anchor@version\|alias]]`; reports validation errors without raising |
| `expand` | `page_id: str`, `anchor?: str`, `version?: str` | `{frontmatter, body, blocks: [{anchor, content}], wikilinks_out: [...]}` | kind / time | the body fetch; `version` is honoured only for instances whose backing store is versioned |
| `neighbours` | `page_id: str`, `direction: 'in'\|'out'\|'both' = 'both'`, `link_skill?: str` | list of `{src, dst, link_skill, link_version?, anchor?}` | kind / time | the link-graph primitive; covers both backlinks and forward-links; time-axis traversal (`prev_review`, `supersedes`) uses the appropriate `link_skill` |
| `list_skills` | — | list of `{id, description, required_frontmatter, optional_frontmatter}` | kind | the Tier 1 catalogue; semantically a `search(*, page_type='skill')` shortcut |
| `list_instances` | `skill_id: str`, `filter?: {frontmatter clauses}`, `order_by?: str`, `limit?: int` | list of `{id, frontmatter}` | kind / time | `search`-shaped shortcut over `neighbours(skill, link_skill=skill)`; supports e.g. `{status: open}` to enumerate open decisions, `{prev_review: null}` to find the head of an append-only chain, or `{at: '>= 2026-04-01'}` plus `order_by='at desc'` to scan an event-typed skill's recent log |
| `run_stored_query` | `query_id: str`, `params?: {…}` | `{rows: [...], schema: [...], snapshot_version?}` | origin | resolves `[[query::query_id]]` and executes against the query's declared `db:` (relational or ext); parameters bound as typed values per the query's `params:` schema (see §"Query parameters in detail") |
| `validate` | `content: str`, `as_page_id?: str` | `{issues: [...]}` | write | dry run; same issue list as `update_page`/`apply_op` but no commit. Used for authoring feedback. |
| `open_session` | `page_id: str` | `{session, head_version, content}` | write (live) | open a CRDT session; agent enters the live-edit lane (see §"Write path in detail") |
| `apply_op` | `session: str`, `op: CRDTOp` | `{ok, conflicts?: [...]}` | write (live) | apply a single CRDT op; concurrent merges are handled by Loro |
| `close_session` | `session: str`, `commit: bool = true` | `{final_version, issues}` | write (live) | materialise the CRDT state to canonical markdown |
| `update_page` | `page_id: str`, `content: str`, `base_version?: str` | `{ok, issues, new_version}` | write (fallback) | whole-page write for environments without CRDT op stream support (no CSP fix); server diffs against current CRDT state and applies as ops |

### Ops / debugging (CLI-only, not exposed over MCP)

Four tools, called by operators or by the build pipeline.

| tool | purpose |
|---|---|
| `audit` | drift detection: returns two sets — markdown-not-in-duckdb, indexed-but-no-markdown |
| `rebuild` | recovery: drops the affected stores for `scope` (whole tenant or page subset) and re-runs the indexer from canonical markdown. Cost ~32 ms/page (T2 [6]) |
| `attach_external` | wire a DuckLake (or other) catalog read-only into the per-tenant `external.ducklake` ATTACH point. Required before `[[table::ext.*]]` resolution works |
| `export` / `import` | per-tenant directory tar+restore (F2 / T5); the unit of backup IS the tenant dir |

The CLI also exposes every agent-facing tool, so an operator can
script the same primitives the agent uses (this is what the
real-LLM run did via `agent_tools.py`).

### What the MCP surface deliberately does NOT expose

- **No direct SQL.** Agents reach the relational store only through
  `run_stored_query` (which dispatches to a `[[query::*]]` instance
  authored ahead of time, with declared schema). This is the H-SECURITY-1
  pattern: SQL is sandboxed AND author-mediated.
- **No raw vector access.** Agents call `search`, not `embed` and
  not `vector_search` directly. The embedding model is an
  implementation detail.
- **No cross-tenant operations.** Each MCP server instance is
  scoped to one tenant; cross-tenant federation is a separate
  layer (F1 / G1).

## The meta-skill: `kb` (a.k.a. how to use this KB)

Every tenant ships with one mandatory skill page whose id is
literally `kb`. The agent loads it once per session via the
same `list_skills` path as any other skill. The skill body
teaches the agent the disclosure model and the tool surface.

Proposed contents:

```yaml
---
type: skill
id: kb
description: How this knowledge base is organised and how to navigate
  it. Read this first when entering a new tenant.
required_frontmatter: []
optional_frontmatter: []
---

# kb — how this knowledge base is organised

This tenant follows the skill::instance pattern. Every page is
either a SKILL (a type declaration) or an INSTANCE (a memory of
that type). Skill pages declare what their instances look like
via `required_frontmatter`. Instance pages declare which skill
they conform to via `skill: <skill-id>`.

## Discovery (start here)

Two valid policies — pick whichever fits the task:

- **Catalogue-first.** Call `list_skills`. The result is the full
  Tier 1 catalogue of `(id, description)` for every skill in this
  tenant. Match your question against the descriptions; descend
  to instances via `list_instances(<skill>)`. ~1 call per
  cold-start question.

- **Search-first.** Call `search(<question>)`. The top hit
  returns its `skill:` field; if the hit is an instance, you have
  identified the skill via 1 call (plus optionally `expand` to
  read the body). If the hit is a skill page, you're done in 1
  call. ~2 calls per cold-start question.

Either way, do NOT preload page bodies for discovery. Tier 1
description-only retrieval clears 83% top-1 on a 169-page
corpus (HYP-E1); body retrieval is a deliberate `expand` once
relevance is established.

## Navigation (after you've found a starting point)

The graph is fully connected through typed wikilinks. From any
page:

- `neighbours(page_id)` — the typed/bare links pointing TO and
  FROM that page, with `link_skill` carried in each row.
- `neighbours(page_id, link_skill=<S>)` — only links cited
  through skill S. "Who cites Acme through a decision-record?"
  is one call.
- `expand(page_id)` — the full body, including all outbound
  wikilinks. Use sparingly; descriptions usually suffice.

## Time axis

The time axis has two sub-axes: what happened (the event log)
and what is true (state at time T). Four patterns are
recognised by convention; none needs a special tool.

- **Event log.** Skills whose `required_frontmatter` includes
  `at:` are **event-typed**. Common members: `meeting`,
  `email`, `call`, `message`, `incident`, `commit`, `release`.
  Each event instance cites the entities it affects through
  ordinary typed wikilinks (e.g. `with: [[customer::acme-corp]]`,
  `participants: [...]`, `about: [[project::phoenix]]`). To
  reach an entity's timeline, call
  `neighbours(<entity>, link_skill IN (<event-skills>))` and
  sort by `at`. Use `list_instances(<event-skill>,
  filter={at: '>= 2026-04-01'})` to filter by date. Events are
  *immutable* by convention; corrections are new event
  instances with a `corrects: [[event::prev-id]]` link rather
  than edits to the original.
- **Append-only chains.** A skill whose
  `optional_frontmatter` includes `prev_<X>` (e.g.
  `prev_review`) carries a chain. The head is the instance with
  no inbound `prev_X` link. Walk backwards via
  `neighbours(head, link_skill=<skill>)`. A skill may be event-typed
  AND chain-typed at the same time (e.g. `weekly-review` has
  both `at:` and `prev_review:`); the two patterns coexist.
- **Supersession.** A skill whose `optional_frontmatter` includes
  `supersedes` carries mutable state. "Current state" =
  instances with no outbound `supersedes` chain pointing TO them.
  Events often *cause* a supersession (the QBR meeting's
  `follow_ups: [[decision-record::...]]` is the typical
  pattern); the system records both — the event and the state
  change — without deriving one from the other.
- **Snapshot pinning.** For external versioned tables (`table`
  instances with `versioned: true`), pin a snapshot at the link
  site: `[[table::customers.churn@v14]]`. The `@version` segment
  is honoured by `resolve` and `expand` only for versioned
  instances; markdown instances ignore it silently.

## Origin axis

External structured data lives as instances of two existing
skills:

- `[[table::<id>]]` — the body describes the table; the
  frontmatter declares `catalog`, `schema`, `name`, and
  `versioned`. Use `expand` to read the schema documentation;
  use `run_stored_query` to read the data.
- `[[query::<id>]]` — the body declares an SQL view. Run via
  `run_stored_query(<id>)`. The query's frontmatter declares
  `db` (`relational` for internal, `ext` for the attached
  DuckLake catalog) and an optional `params` schema.

There is no separate tool for external data. You traverse it
with the same primitives.

## Writing

Two modes, both backed by the same CRDT layer (Loro) under
the hood:

- **Live mode.** `open_session(page_id)` returns a session
  token + the current head state. Issue `apply_op(session,
  <op>)` for each granular edit; the server merges concurrent
  edits from other actors and returns the merged content on
  each response. `close_session(session, commit=true)`
  materialises the state to canonical markdown and triggers
  the indexer. Use this when you're co-editing with a user or
  another agent.
- **Fallback (whole-page) mode.** `update_page(page_id,
  content, base_version=<v>)` submits the full markdown body.
  The server diffs against current CRDT state and applies as
  ops. If concurrent edits make a clean merge impossible, the
  call returns `{ok: false, issues: [{code: 'conflict',
  ...}]}` with the new content for you to re-draft. Use this
  when the MCP transport does not support op streaming, or
  when you're authoring a whole new page from scratch.

Before either, call `validate(<content>)` to see what the
indexer would say without committing. All three (`validate`,
`apply_op`, `update_page`) return the same issue shape:

```
{severity: 'error'|'warning', code: <str>, location: <str>,
 message: <str>, suggestion?: <str>}
```

An `error`-severity issue rejects the write;
`warning`-severity issues commit but appear in the response.
The codes are documented in `[[error-catalogue]]` (one bare
wikilink — the catalogue is itself a `note` instance).

## Tool surface summary

| tool | what for |
|---|---|
| `search` | natural-language → top-K hits (block or page granularity via `granularity=`) |
| `resolve` | `[[wikilink]]` → `(page_id, skill, exists)` |
| `expand` | page id → body + blocks + wikilinks_out |
| `neighbours` | graph traversal, typed-filterable |
| `list_skills` | the Tier 1 catalogue |
| `list_instances` | enumerate instances of a skill, optional frontmatter filter, optional ordering (`order_by`) — typical event-log call is `list_instances('meeting', filter={at: '>= 2026-04-01'}, order_by='at desc')` |
| `run_stored_query` | execute a `[[query::*]]` instance with typed parameters |
| `validate` | dry-run the indexer's checks on a draft |
| `open_session` / `apply_op` / `close_session` | live CRDT editing (when the MCP client supports the op stream) |
| `update_page` | whole-page write fallback (no CRDT op stream required) |

## Anti-patterns

- Do NOT call `expand` on every search hit; it's the most
  expensive primitive. Descriptions in the search result are
  usually enough.
- Do NOT enumerate the whole catalogue if the task is narrow;
  search-first reaches the right skill in 2 calls.
- Do NOT write raw SQL through `run_stored_query` — the dispatcher
  refuses queries that aren't `[[query::*]]` instances. Author
  the query as a page first, then call it.
- Do NOT trust a page body's mention of an entity over a typed
  wikilink. `mentions: [Acme]` in frontmatter is an unvalidated
  freeform string; `[[customer::acme-corp]]` is a validated
  citation.
```

The `kb` skill is **the only mandatory skill** in a tenant. Every
other skill is optional and tenant-specific.

## How a session unfolds

A typical agent session against this surface:

1. **Boot.** Agent connects to the MCP server. No discovery call
   yet — the agent has a task in hand.

2. **First call.** Either `list_skills` (catalogue-first) or
   `search(<task>)` (search-first). Either yields enough to
   identify the relevant skill.

3. **Identify the relevant instance(s).** Either
   `list_instances(<skill>)` (if the task names the type), or
   the search hits already point at specific instances.

4. **Selective expansion.** `expand` the candidates that the
   description suggests are relevant. Most tasks need 1–3
   `expand` calls.

5. **Traverse the graph if needed.** `neighbours(page, link_skill=<...>)`
   to follow typed links — get all decisions citing a customer;
   get all weekly-reviews citing a person; walk the
   `prev_review` chain back six weeks.

6. **Execute stored queries if needed.** `run_stored_query` for
   anything that needs the relational or external lane (e.g.
   "the trend in Acme's churn score over Q1").

7. **Write back if the task is editorial.** `validate` the draft;
   if clean, `update_page`. If issues, fix them based on the
   issue codes.

The real-LLM run measured the read path: median 1–2 tool calls
for discovery, 2–7 calls for closed-loop tasks like "list open
decisions citing Phoenix". The write path is not yet measured
end-to-end; the H-AUTHORING-1 engine-path verification confirms
the issue codes are produced correctly.

## Where this design diverges from what already exists

Comparing to the prototype's current `Tools` class
(`2026-05-16-kb-e2e-skills-prototype/src/kb.py` plus the
real-LLM driver `agent_tools.py`):

| prototype today | this design | reason |
|---|---|---|
| `vector_search(q, k)` | `search(q, k, page_type?, skill?)` | needs `page_type` filter for the search-first cold-start path; needs `skill` filter for typed retrieval |
| no `resolve` | `resolve(wikilink)` | the agent needs to programmatically check whether a link is valid before traversing |
| `read_page` | `expand(page_id, anchor?, version?)` | rename for clarity (matches §3) and add the time-axis `version?` argument |
| no `neighbours` (only `backlinks` + `forward_links`) | `neighbours(page_id, direction, link_skill)` | unify into one symmetric primitive |
| no `validate` | `validate(content, as_page_id?)` | required for the authoring feedback path (H-AUTHORING-1 engine path); cheap to add |
| no write path | `open_session`/`apply_op`/`close_session` (live) + `update_page` (fallback) | full CRDT op-stream write per locked decision (1); whole-page fallback for environments without CSP fix |
| no `run_stored_query` over MCP yet (only Python) | expose it with parameter binding | the origin axis depends on it; parameters per locked decision (2) |
| no `kb` meta-skill in the corpus | add it, mandatory + auto-shipped | per locked decision (3); the agent has no in-corpus documentation of the tool surface today |
| `search` returns blocks only | `search(..., granularity='block'\|'page')` | both granularities exposed per locked decision (4); block remains the default |

None of these are large lifts. The biggest addition is the `kb`
meta-skill, which is just one markdown file.

## Decisions locked (2026-05-17)

The four open questions above were resolved in the design
session:

1. **Write path: full read+write via CRDT op stream over MCP.**
   The agent participates in the same CRDT layer as the web
   client. Operational consequence: this design has a hard
   dependency on the Claude.ai CSP fix
   (`anthropics/claude-ai-mcp#40`, see HYP-D1). For
   environments where the CSP fix is not yet available, the
   whole-page `update_page(...)` fallback is preserved as a
   degraded mode. See §"Write path in detail" below.

2. **`run_stored_query` is parameterised.** Query instances
   declare a `params:` schema in frontmatter; the dispatcher
   binds parameters safely. See §"Query parameters in detail"
   below.

3. **The `kb` meta-skill is mandatory and auto-shipped.**
   The indexer ships `skills/kb.md` with every new tenant.
   Tenants may extend the page with tenant-specific guidance
   (appended after the standard sections) but cannot delete
   it or remove the standard sections.

4. **`search` returns blocks OR pages, agent chooses via flag.**
   Default is `granularity='block'` (matches HYP-E2 and the
   real-LLM run); `granularity='page'` is available for tasks
   where the agent wants page-level summary hits.

The tool surface table above is updated to reflect these
decisions; the detail sections below specify the parts that
need it.

## Write path in detail

The agent's write surface is three CRDT-aware tools plus the
whole-page fallback:

| tool | inputs | output | mode |
|---|---|---|---|
| `open_session` | `page_id: str` | `{session: str, head_version: str, content: str}` | live |
| `apply_op` | `session: str`, `op: CRDTOp` | `{ok, conflicts?: [...]}` | live |
| `close_session` | `session: str`, `commit: bool = true` | `{final_version, issues}` | live |
| `update_page` | `page_id: str`, `content: str`, `base_version?: str` | `{ok, issues, new_version}` | fallback |

**Live mode (the default once the CSP fix lands).** The agent
calls `open_session(page_id)` to get a CRDT session token plus
the current head state. The agent then issues `apply_op(...)`
for each granular edit (insert / delete / move). Concurrent
edits from other actors are merged by Loro (B1 [verified]); the
agent receives merged content on each `apply_op` response and
can adapt mid-stream. `close_session(commit=true)` materialises
the final state to canonical markdown and triggers
`update_page`-equivalent indexer work.

The `op` schema is whatever Loro emits — opaque to the agent
in detail but always parseable as one of `{insert, delete,
move, mark, unmark}` against a block path. The agent generally
does not handcraft ops; the typical pattern is for the agent
to receive an op stream from the harness's text-edit tool
(which generates ops from agent intent) and forward them.

**Fallback mode.** Where the CSP fix is not deployed (today,
in many MCP clients), the agent calls `update_page(page_id,
content, base_version=<v>)` with the full markdown body and
the version it based its draft on. The server diffs against
current CRDT state, produces the op set, and applies. If
concurrent edits have changed `base_version`, the server
either auto-merges (Loro's CRDT guarantee) or returns
`{ok: false, issues: [{code: 'conflict', ...}]}` for the
agent to retry with the new content.

**Validation runs on every write.** Both `apply_op` and
`update_page` enforce the index-time checks (skill exists,
target exists, anchor exists, frontmatter required keys
present). An `error`-severity issue rejects the op or page;
`warning`-severity issues commit but appear in the response.
The issue codes are the ones H-AUTHORING-1 verified.

**Awareness is a separate channel.** Viewer presence and
cursor positions (per C-9 Model B) do not flow through these
tools. They use a polled heartbeat over the existing MCP
transport, not the CRDT op stream.

## Query parameters in detail

A `[[query::*]]` instance declares its parameter schema in
frontmatter:

```yaml
---
type: instance
skill: query
id: customer-churn-trend
db: ext
params:
  - {name: customer_id, type: text, required: true}
  - {name: from_date,   type: date, required: false, default: '2026-01-01'}
  - {name: to_date,     type: date, required: false}
sql: |
  SELECT as_of, score
  FROM ext.customers.churn
  WHERE customer_id = :customer_id
    AND as_of >= :from_date
    AND (:to_date IS NULL OR as_of <= :to_date)
  ORDER BY as_of
---
```

Calling pattern:

```
run_stored_query("customer-churn-trend",
                 params={"customer_id": "acme-corp"})
  → {rows: [...], schema: [...], snapshot_version: 27}
```

Type enforcement happens at the dispatcher: parameters are
bound as typed values (DuckDB prepared statements), never as
string interpolation. This closes the SQL-injection class that
H-SECURITY-1 covered for stored SQL but did not exercise for
parameter binding.

Missing required parameters return
`{error: 'missing_required_param', name: 'customer_id'}`
without dispatch. Extra parameters return
`{error: 'unknown_param', name: 'xyz'}` — strict matching to
the declared schema.

The `snapshot_version` field is populated only for queries that
hit a versioned external table (DuckLake). It pins the
agent's view; if the agent caches the result and wants the
same view later, it can pass `as_of_snapshot=<version>` to a
follow-up call to guarantee the same data.

## `search` granularity in detail

Two granularities, same `search` tool:

```
search(q, k=10, granularity='block', page_type?, skill?)
  → [{page_id, anchor, snippet, skill, page_type, score}, ...]

search(q, k=10, granularity='page', page_type?, skill?)
  → [{page_id, best_anchor, snippet, skill, page_type, score,
      block_count}, ...]
```

Block granularity (default) is what the real-LLM run used:
each hit pinpoints a specific block within a page. Good for
H5-style citation retrieval ("which block in which page
mentions Porter stemming").

Page granularity collapses adjacent block hits into one
per-page row, with the best-scoring block returned as the
snippet. Good for "enumerate the pages broadly related to X"
tasks where the agent doesn't need a specific block.

The choice is recorded in the response so the agent (and any
caching layer) can tell them apart.

## Tier 1 cost of the auto-shipped `kb` skill

The mandatory `kb` skill adds one entry to every tenant's Tier
1 catalogue. Measured cost: ~180 tokens for the skill page's
`(id, description)` line in `list_skills`. The skill body
(the larger doc, ~6 k tokens) loads on demand via `expand` —
exactly like every other skill body. Net effect on the Tier 1
budget arithmetic from HYP-D2: negligible (~0.3%).

## Remaining open questions

Two questions deferred because they're more about deployment
than design contract:

- **CRDT op stream over MCP transport: is the right shape
  request/response (each `apply_op` is one HTTP call) or
  bidirectional streaming (one long-lived call carries many
  ops)?** Streaming is more efficient but few MCP clients
  implement it today. Recommended: ship request/response first,
  add streaming when there's a measurable benefit.
- **Subscription / awareness channel: does the same MCP server
  host both, or is the awareness channel a separate
  endpoint?** Single endpoint is simpler operationally but
  couples the two concerns. Separate endpoint matches the C-9
  recommendation that awareness is a cheaper channel than
  data merge.

These can be punted to implementation.
