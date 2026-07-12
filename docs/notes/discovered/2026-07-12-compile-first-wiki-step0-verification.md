# Step-0 verification for the compile-first-wiki work (G1/G2/G3/G4)

Before designing the three-then-four "compile-first wiki" operations
(integrative distillation, semantic lint, freshness+curation, and
eval-driven improvement — see [`../../contract/compile-first-wiki.md`]), the
framing handed to us was checked against the actual code. This note records
what held and what was stale, so nobody codes against a false premise.

## Symptom / why this note exists

The task framing was derived from design specs and was **stale against a
worktree that was 13 commits behind `origin/main`**. Read literally against
that local `HEAD` (`28faa46`) it says "the `kind: workflow` reducer does not
exist yet; scope a minimum reducer." That conclusion is wrong for the
merged tree.

## The one finding that reframes everything

**The entire dynamic-workflows program — PRs #254–#266 — has already merged
to `origin/main`.** The reducer that generalizes `emit_cascade` to width>1 +
join is fully built:

- `crates/escurel-runner-workflow/src/{spec,step,reduce,barrier,budget,key,corpus}.rs`
- `crates/escurel-runner-core/src/workflow.rs` (the I/O driver, `drive_workflow`)
- `reduce(spec, state) -> Vec<StepIntent>` does `fan_out: {over}` (width>1),
  `Fixed` barrier points, quorum-verify with terminal closure, `pipeline()`
  semantics, budget reservation, and workflow-aware crash recovery.
- Wired into the dispatch loop at `crates/escurel-runner/src/main.rs:970`
  (`drive_workflow` replaces `emit_cascade` for workflow-labelled runs).

So **no new reducer is needed** for integrative distillation. The work is
corpus (new `kind: workflow` skills) plus one *scoped* reducer extension for
writing durable existing pages (below).

## Confirmed (framing correct)

- **Cascade** is width-≤1, cross-skill-only, join-free, fires only after a
  confirmed write; `update_page` emits no event (`cascade.rs:79-124`, dispatch
  `main.rs:1002`).
- **No scheduler exists.** The only periodic tick is the inbox poller
  (`main.rs:1232`, 30s), a webhook backstop that re-pulls the inbox — it never
  synthesizes non-inbox work. A "scheduled" lint pass has no existing
  foundation and had to be designed.
- **Agent tool surface** (`escurel-server/src/mcp.rs:1057`): `search`,
  `resolve`, `expand`, `validate`, `update_page`, `capture_event`,
  `query_instance`, `list_instances`, `neighbours`, `list_skills` are all
  `Role::Agent`-available; `run_stored_query` **is admin-gated**
  (`mcp.rs:1065-1072`). Fail-closed per-instance ACL in `acl.rs` (pure
  owner-field comparison, no LLM in the authz path).
- **Links graph** supports orphan detection: `links(src_page, …, dst_page, …,
  link_skill, …)` (`sql/0001_b_tables.sql:21`); `is_cited`
  (`citation.rs:66`) is the inbound-existence primitive; the agent-facing
  `neighbours` tool (`read.rs:656`) exposes in/out edges without admin SQL.
- **Frontmatter** is one `pages.frontmatter JSON` column queried via
  `json_extract_string`; `list_instances` filters a single
  `frontmatter_key`/`frontmatter_value` equality (`read.rs:275`). Verbatim,
  schemaless YAML → new fields need no migration. `frontmatter_index` is dead
  (see the sibling note) — do not reference it.

## Corrected (stale or imprecise premises)

- **`create_instance` is not an agent tool.** Agents create instances via
  `update_page` to a fresh page id (or `capture_event` → the runner). MCP
  `create_*` variants are all admin-gated.
- **`list_instances(skill, {filter})`** is imprecise: the arg is `skill_id`
  and the filter is a single `frontmatter_key`/`frontmatter_value` pair (the
  `{filter}` object form is on `search`).
- **There is no persisted typed-Issue channel.** `Issue` (`validate.rs:61`)
  is ephemeral — `{severity, code, location, message, suggestion}` returned by
  the `validate` tool, never stored. `audit()` returns structural `AuditDrift`
  (markdown↔duckdb), admin-gated — derivability, not semantic health. The lint
  work therefore introduces an `issue` typed instance-kind as the stored home.
- **Two corpus-shipping mechanisms, not one.** *Mandatory* (`ensure_meta_skill`
  at boot, `config.rs:963`) vs *opt-in library* (`deep_research_corpus()`
  returns `(page_id, markdown)` pairs a tenant seeds — NOT auto-shipped). The
  compile-first corpus ships **opt-in**, so markdown-only tenants are
  unaffected.
- **`escurel-eval` scores retrieval, not answers, and never writes back.** It
  is a standalone CLI IR harness (BEIR `corpus/queries/qrels`, nDCG/recall/MRR/
  MAP + a threshold gate); nothing imports it and no loop feeds results into
  the corpus. Eval-driven improvement is genuinely net-new; the qrels
  (`corpus _id == page_id`) are a ready-made per-page diagnosis substrate.
- **Skill pages are editable.** Only the mandatory `escurel` meta-skill is
  write-protected (`indexer.rs:339`, append-only); every other `type: skill`
  page is freely editable via `update_page`. So an improvement loop may refine
  skills, the connective tissue.
- **No `CHANGE-REQUEST.md`/`HLD.md` convention.** Spec-first docs live under
  `docs/adr/`, `docs/spec/`, `docs/contract/`; the freshest template is
  `docs/contract/dynamic-workflows.md`.

## The one real design constraint carried into G1

The reducer matches produced instances to steps by exact pre-flagged
**run-scoped** page ids — `phase_complete` compares `list_instances(<produces>)`
against `intent.instance_page_id()` = `markdown/instances/<produces>/<run_slug>-<slot>.md`
(`reduce.rs:140-172`). A step that writes directly to a durable *existing*
entity page would break completion detection. The design resolves this with a
scoped `writes: existing` + `target_field:` reducer extension whose completion
signal is a `source_event` frontmatter stamp on the durable target (see the
contract doc, G1).
