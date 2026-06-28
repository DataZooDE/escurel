---
type: skill
id: escurel
description: How this knowledge base is organised and how to navigate
  it. Read this first when entering a new tenant.
required_frontmatter: []
optional_frontmatter: []
---

# escurel — how this knowledge base is organised

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

## Searching effectively

`search` runs a hybrid lane (dense embedding + BM25). A few habits
make it land on the right passage instead of a near-miss.

- **Rewrite before searching.** Expand abbreviations, spell out
  acronyms, and add likely synonyms before you query. Prefer the
  corpus's own vocabulary: if the tenant says "churn risk", search
  that, not "customers about to leave". A terse user question is
  rarely the best query string.
- **HyDE for conceptual questions.** When the question is about a
  concept rather than a known token, write a hypothetical 1–2
  sentence *answer* and search with that instead of the question.
  The embedding of a plausible answer sits nearer the target
  passages than the embedding of a short question does.
- **Multi-query for broad or ambiguous questions.** Issue 2–3
  different phrasings (e.g. a literal one, a synonym-rich one, a
  HyDE answer) and merge the hits. One phrasing risks anchoring on
  a single sub-topic; several widen recall.
- **Use exact tokens for exact things.** Pass product codes, page
  ids, person/company names, and error codes verbatim — do not
  paraphrase them. The BM25 lane fires on exact tokens, so a
  literal `ESC-4012` or `acme-corp` retrieves the precise instance
  that a fuzzy description would miss.

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
