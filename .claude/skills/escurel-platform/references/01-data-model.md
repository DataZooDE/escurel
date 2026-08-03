# 01 — The data model: designing your tenant

This is how you model your domain in Escurel. Canonical source:
`docs/contract/agent-interface.md`. A worked example tenant lives at
`examples/crm-demo/` (a small CRM: `customer`, `contact`, `lead`,
`opportunity`, `engagement`, `project`).

## Skills and instances

A **skill** page is a type declaration:

```markdown
---
type: skill
id: customer
description: A buying organisation tracked by the sales team.
required_frontmatter: [name]
optional_frontmatter: [primary_contact, tier]
---

# customer

The `customer` skill represents a buying organisation. …
```

An **instance** page is a memory of that type:

```markdown
---
type: instance
skill: customer
id: acme-corp
name: Acme Corp
primary_contact: "[[contact::we-coyote]]"
---

# Acme Corp

Acme Corp is a long-standing customer … primary contact is W. E. Coyote.
```

Frontmatter rules the indexer enforces at write time:
- `type:` is `skill` or `instance`.
- A skill declares `id`, `description`, and the
  `required_frontmatter` / `optional_frontmatter` lists.
- An instance declares `skill:` (the skill it conforms to), `id`, and
  every key in that skill's `required_frontmatter`.
- A missing required key is an **error**-severity validation issue and
  rejects the write (`references/02` §validation; `references/07`).

Two frontmatter fields are **server-governed** — your app never writes
them: `layer:` (stamped by pack import; a draft declaring `layer: base@…`
is rejected `layer_read_only`) and `promotable:` (curator/admin-set; a
non-admin write carrying it is rejected `promotable_requires_curator`).
See §Layer/stability axis below.

See the live shapes in `examples/echo-app/tests/fixtures/customer.skill.md`
and `…/acme-corp.md`.

## Typed wikilinks

Pages connect through wikilinks. Full grammar (all segments after `id`
optional):

```text
[[skill::id]]
[[skill::id#anchor]]
[[skill::id@version]]
[[skill::id|alias]]
[[skill::id#anchor@version|alias]]
```

A wikilink is a **validated citation** — the indexer checks the target
exists. A freeform `mentions: [Acme]` string in frontmatter is *not* a
citation; never treat one as a link. The link's `skill` segment is its
`link_skill`, which is what `neighbours(..., link_skill=…)` filters on.

## The three axes — same primitives, no special tools

- **Kind axis.** "What type is this?" → `list_skills`, `list_instances`,
  `search(..., page_type=…, skill=…)`.
- **Time axis.** Two sub-axes, four conventions, *no special tool*:
  - **Event log** — skills whose `required_frontmatter` includes `at:`
    are event-typed (`meeting`, `email`, `incident`, …). Events cite the
    entities they affect via wikilinks; reach an entity's timeline with
    `neighbours(entity, link_skill IN (<event-skills>))` sorted by `at`,
    or `list_instances(<event-skill>, order_by_at='desc')`. Events are
    immutable by convention; corrections are new events with a
    `corrects: [[…]]` link.
  - **Append-only chains** — a skill with `prev_<X>` in
    `optional_frontmatter` (e.g. `prev_review`). The head has no inbound
    `prev_X`; walk back via `neighbours(head, link_skill=<skill>)`.
  - **Supersession** — a skill with `supersedes` in `optional_frontmatter`
    carries mutable state; "current" = not superseded by anything.
  - **Snapshot pinning** — `[[table::x@v14]]` pins a version on a
    versioned external table; ignored for markdown instances.
- **Origin axis.** External structured data lives as instances of two
  built-in skills:
  - `[[table::<id>]]` — frontmatter declares `catalog`, `schema`, `name`,
    `versioned`. `expand` reads the schema doc; `run_stored_query` reads
    the data.
  - `[[query::<id>]]` — body declares an SQL view; frontmatter declares
    `db` (`relational` | `ext`) and a typed `params:` schema. Run via
    `run_stored_query(<id>, params)`. See `references/02` §run_stored_query.
- **Backend axis.** A skill may declare an **instance backend** in its
  frontmatter (`backend: { kind: … }`), so its *instances* are sourced from
  outside markdown. `list_skills` reports each skill's `backend.kind`
  (`markdown` | `sql_view` | `document`) and a `capabilities` object. You read
  these instances with the same primitives, but `sql_view` and `document` are
  **read-only** (`capabilities.writable == false`; `update_page` → `backend_read_only`):
  - `sql_view` — projects a read-only DuckDB view over an external relational
    source; `expand` returns the overlay + a bounded row projection
    (`backend_projection`). Created with `create_sql_instance`; secrets via
    `register_credential`; drift checked with `validate_bindings`.
  - `document` — an uploaded PDF/DOCX/PPTX/XLSX/text file, extracted + chunked +
    embedded into a page-with-chunks. Uploaded via `POST /ingest` /
    `POST /ingest/upload`; `expand` returns the overlay + top-k chunks
    (`chunks_total`), never the full text. See `references/02` §instance-backends.

## Layer/stability axis

Orthogonal to its backend, every page carries a stability **layer**
(canonical: `docs/contract/agent-interface.md`, ADR-0005/0007):

- **`overlay`** — tenant-authored, editable; the default (every page with
  no `layer:` frontmatter is an overlay page).
- **`base@<pack>@v<N>`** — imported from a subscribed **skill pack**,
  **read-only at this node**. Base pages live under the reserved
  `markdown/base/` page-id namespace; `update_page` against one (or any
  id under that prefix) returns `layer_read_only`, and `open_session`
  fails with a JSON-RPC `-32000` error prefixed `layer_read_only:`.
  Stripping `layer:` from a draft is not an unlock — the guard keys off
  the *stored* page — and a draft *declaring* `layer: base@…` is rejected
  the same way.

`list_skills` reports each skill's `layer`, so an agent can tell the
stable, firm-authored substrate from the tenant's own editable pages.

**Shadowing** — how a tenant specialises a base skill *without* editing
it: author an overlay skill page declaring the **same skill id**
(curator/admin only — a non-admin write refuses
`shadow_requires_curator`). `resolve` then prefers the overlay;
`list_skills` shows ONE entry for the id carrying `layer` plus a
`shadows: base@<pack>@v<N>` pin; `expand` of the shadowing overlay
carries a `shadow` object (`{base_page_id, pack, base: {…the base page's
frontmatter…}}`) so the base values stay visible, never silently masked.
The base page itself is untouched.

**`promotable: true`** is the curator-set marker that makes a
tenant-authored skill page eligible for promotion back to the hub
(`submit_promotion`, admin-gated — `references/02` §Skill packs). A
non-admin `update_page` whose draft carries a truthy `promotable`
refuses (`promotable_requires_curator`).

## The mandatory `escurel` meta-skill

Every tenant ships one mandatory skill page whose `id` is literally
`escurel`. It teaches a **runtime LLM agent** the disclosure model and
tool surface (catalogue-first vs search-first, Tier-1 cheap / Tier-2
lazy). It is auto-shipped at tenant creation; tenants may *append*
tenant-specific guidance but cannot delete it or the standard sections.

You usually don't author it by hand — it ships with the tenant. When you
seed a fresh tenant in tests, include it if your test exercises an agent
that loads it. Worked example: `examples/crm-demo/skills/escurel.md`.

Do not confuse it with *this* `escurel-platform` skill: the meta-skill is
content inside a tenant for runtime agents; this skill is developer
documentation for building the app.

## Designing your tenant — checklist

1. Enumerate your entity types → one **skill** page each, with tight
   `description`s (they are the Tier-1 catalogue an agent matches against).
2. Decide each skill's `required_frontmatter` (the contract) vs
   `optional_frontmatter` (including any `at:`, `prev_*`, `supersedes`
   that opt the skill into a time pattern).
3. Express relationships as typed wikilinks, not freeform strings.
4. Put external/relational data behind `table` / `query` instances; never
   expose raw SQL to the app or agent.
5. Seed representative instances as fixtures for your integration tests
   (`references/06`, `07`).
