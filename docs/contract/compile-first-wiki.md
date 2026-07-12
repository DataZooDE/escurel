# The compile-first wiki in escurel — a concept

**Date:** 2026-07-12.
**Status:** Concept / design proposal, accepted for phased delivery.
**Scope:** Four operations that turn escurel from a retrieval-first KB into a
self-maintaining, **compile-first "LLM Wiki"** (Karpathy pattern) — *integrative
distillation*, *semantic lint*, *freshness + curation*, and *eval-driven
improvement* — each expressed in escurel's own idioms (skills + instances +
events, the reactive runner, typed Issues, fail-closed ACL) with **no scripting
runtime and no new ingress path**.

This is a concept, not a spec. It extends [`agent-orchestration.md`] and builds
**on top of** the dynamic-workflows reducer ([`dynamic-workflows.md`], merged as
PRs #254–#266) — these are missing *operations*, not missing architecture. The
grounding sweep behind every claim here is [`../notes/discovered/2026-07-12-compile-first-wiki-step0-verification.md`].

---

## TL;DR

- escurel already distills: an inbox event is a raw source; an agent reacting to
  it and writing an instance is the distillation; a confirmed cross-skill write
  fires `emit_cascade` (one follow-on event, width ≤ 1, no join). The
  dynamic-workflows reducer already generalizes that to width>1 + a quorum
  barrier. What is missing are four **operations this loop does not yet perform**.
- **G1 — integrative distillation (breadth).** A `distill` `kind: workflow`
  whose steps weave one source's claims into *many existing* entity/concept
  pages. Needs one scoped reducer extension (`writes: existing`) so a step can
  target a durable page instead of a run-scoped one.
- **G2 — semantic lint.** A scheduled `lint` `kind: workflow` that flags
  contradictions, stale claims, orphan pages, and missing cross-refs as **typed
  `issue` instances**. It *proposes*; it never rewrites. Scheduling is a new
  internal runner tick (no scheduler exists today).
- **G3 — freshness + curation.** First-class page freshness frontmatter
  (`last_verified`/`source_event`) so stale knowledge surfaces, plus a curated,
  by-category `index` instance — a "map of the territory," kept derivable.
- **G4 — eval-driven improvement (the reinforcement half).** An `eval`
  `kind: workflow` scores how well the KB answers tasks; failures feed an
  `improve` workflow that edits the implicated **documents *and skills***, then
  re-evaluates. Distillation optionally takes input from this eval procedure —
  the wiki learns from task performance, not just from new sources.
- Every piece is **opt-in corpus**: absent the seeded skills (and, for G4, an
  eval dataset), a markdown-only tenant behaves exactly as before.

---

## 1. The escurel mapping (script → triad, again)

Like dynamic-workflows, none of this is a second execution model. Each operation
is a **plan** (a `kind: workflow` skill), whose **runs** are instances, whose
**steps** are events re-entering the existing poll → trigger → package → harness
→ reconcile → drive loop. The only genuinely new machinery is:

1. a **durable-target step** (G1) — a step that pre-flags an existing page;
2. a **runner-side schedule tick** (G2) — the runner synthesizing a periodic
   `capture_event`;
3. a **persisted Issue** kind (G2) — `issue` instances;
4. an **eval → corpus** write-back (G4).

Everything else is reuse: the reducer, the barrier, `admit`'s loop controls, the
packager's narrowed `WORKFLOW_STEP_TOOLS` surface, the ledger, recovery.

## 2. G1 — Integrative distillation (`distill`)

### 2.1 The plan

```yaml
type: skill
id: distill
backend: {kind: workflow}
run_skill: workflow-run
phases:
  - {id: extract,   produces: distill-claim, fan_out: {over: <source>}}
  - {id: match,     produces: weave,         fan_out: {over: distill-claim}}
  - {id: weave,     produces: weave,         fan_out: {over: weave},
                    writes: existing, target_field: target_page, dedup_by: target_page}
  - {id: integrate, produces: distill-report, fan_out: 1}
```

- `extract` fans out over the source's sections/chunks → `distill-claim`s. For a
  `document`-backend source the harness `expand(full)` reads the chunks and
  writes claims from them, so **PDFs are distilled into concept instances**, not
  left inert.
- `match` decides, per claim, which existing entity/concept page it belongs to
  (via `search`/`resolve`), writing a `weave` record `{target_page, claim_ref,
  action: update|create, patch}`.
- `weave` applies the fact to the **durable** `target_page` (creating it when
  `action: create`), and stamps `source_event`/`last_verified` (G3).
- `integrate` (Fixed barrier) marks the source integrated once every weave lands.

### 2.2 The reducer extension (`writes: existing`)

The reducer today pre-flags every step onto a run-scoped page id and detects
completion by that id landing (`reduce.rs:140-172`). A durable page already
exists, so that signal cannot work for a weave. The scoped extension:

- **`spec::Phase`** gains `writes: WriteMode` — `New` (default) or
  `Existing { target_field }`, parsed leniently.
- **`step::StepIntent`** gains `target_page: Option<String>`;
  `instance_page_id()` returns it when set, else the run-scoped pre-flag. The
  content-addressed `event_id()`/`slot` fold in `target_page`, so a re-driven
  weave **overwrites, never forks**.
- **`reduce::plan_phase`** for an `Existing` phase reads each upstream `weave`
  element's `target_field` value as that step's `target_page`.
- **Completion signal (reuses G3).** The weave harness stamps the target page's
  `source_event: <this step's event id>`. The driver (`workflow.rs
  build_run_state`) loads each resolved target page's frontmatter into a new
  `RunState.targets`, and `phase_complete` for an `Existing` phase checks
  `target.source_event == step.event_id()`. Pure, reducer-readable, no event-log
  peek.

**Backward-compatible:** `writes` absent ⇒ `New` ⇒ deep-research and every
existing workflow behave byte-identically; the branch is inert unless declared.
**Single-writer:** `dedup_by: target_page` ⇒ at most one weave per durable page
per run.

A single distill run thus touches **≥2 existing entity pages** — genuine
width>1 breadth, with the reducer's completion correctness preserved.

## 3. G2 — Semantic lint (`lint`) + typed Issues

### 3.1 The persisted Issue

A new `issue` typed instance-kind (the stored companion to the ephemeral
`validate` `Issue`):

```yaml
type: skill
id: issue
required_frontmatter: [kind, severity, subject_page, message]
optional_frontmatter: [suggestion, detected_at, source_run, status]
```

Issues are ordinary instances → derivable, ACL'd, and queryable via
`list_instances(issue, {frontmatter_key: kind})`.

### 3.2 The plan — detect, never rewrite

Phases each `produces: issue`:

- `orphans` — `neighbours` (in-direction) finds pages with no inbound links →
  `issue{kind: orphan}`.
- `missing-xref` — `search` each page's entities; a strong match with no
  `[[link]]` → `issue{kind: missing_xref, suggestion: <wikilink>}`.
- `stale` — a page whose `last_verified` (G3) predates the plan's `stale_after`
  → `issue{kind: stale}`.
- `contradiction` — a Fixed barrier agent pass over topically-clustered /
  recently-touched instances → `issue{kind: contradiction}`.

**Lint proposes, never disposes — three enforcement layers:** (1) every phase
`produces: issue`, so the reconciled output is always an issue; (2) lint steps
run on the packager's `WORKFLOW_STEP_TOOLS` surface — read + `validate` +
`update_page`, with **`capture_event`/`assign_event` denied**; (3) the plan body
instructs "write issues, suggest edits, change nothing else." Disposition is a
later, separate act by an agent with write rights.

### 3.3 Scheduling — an internal runner tick

No scheduler exists, so the runner grows a periodic **lint tick** beside the
inbox poller (`main.rs:1232`). Each interval, per lint-enabled tenant, the
*runner* calls `capture_event(label_skill: lint)` → inbox → the reactive loop
drives the workflow.

- The **runner owns the decision to act** (as it always has for cascades and
  recovery); the **gateway stays automation-free** — it only serves and notifies.
- **Idempotent:** the tick's event id is `blake3("lint", tenant,
  floor(epoch / interval))`, so an overlapping tick or a mid-window restart
  collapses via `capture_event`'s `ON CONFLICT DO NOTHING`.
- **Opt-in:** `ESCUREL_RUNNER_LINT_INTERVAL` defaults to *disabled*; a
  markdown-only tenant never ticks.

## 4. G3 — Freshness + curated index

- **Freshness frontmatter** (written only by distill/lint/verify — markdown
  tenants unaffected): `last_verified` (RFC 3339), `source_event`, optional
  `source_commit`. Set by G1's weave/integrate steps and refreshed when a
  `stale`/`contradiction` issue is resolved. `<`-comparisons for staleness use an
  agent-safe `list_instances` scan + in-Rust date compare (mirroring the barrier
  tally's in-Rust `COUNT`), plus an operator-only inspection `query` instance.
  Threshold = the lint plan's `stale_after` (e.g. `P90D`), auditable in the KB.
- **Curated index** — an `index` skill whose `curate` phase (Fixed, fan_out 1)
  reads `list_skills` + per-skill `list_instances`, groups by category, and
  writes one-line summaries into one `index` instance. Kept in sync via the same
  runner-tick path (a `curate` tick) or as a distill `integrate` follow-on. It
  stays **fully derivable** — regenerable from `pages/` + events.

## 5. G4 — Eval-driven improvement (`eval` + `improve`)

### 5.1 Eval → `eval-result` instances

An `eval` `kind: workflow` runs a task set against the KB and scores it in two
tiers, both emitting persisted `eval-result` instances (`{task, tier, score,
verdict, implicated_pages, implicated_skills, diagnosis}`):

- *retrieval tier* — reuse `escurel-eval`'s metrics per query vs qrels; a
  below-threshold query names the relevant pages that ranked poorly.
- *factual/task tier* — an agent-judge phase (reusing the deep-research verify
  machinery): answer the task from the KB, grade against a golden/rubric,
  diagnose which pages/skills were wrong or insufficient.

`escurel-eval` stays a standalone CLI for offline/CI; G4 adds only a
feature-gated corpus-emitting path (the workflow writes `eval-result`s via
`update_page`), reusing `report::EvalReport` + `gate::GateOutcome`.

### 5.2 The `improve` workflow

Triggered reactively by a failing `eval-result`
(`capture_event(label_skill: improve)`, or the `eval` plan's terminal phase):

- `diagnose` (fan_out over failing `eval-result`s) → a `weave` record with
  `target_page` **or `target_skill`** and the correction.
- `improve` (`writes: existing`, `target_field: target_page|target_skill`) —
  the **same durable-target extension as G1** — applies the fix to the
  implicated document or skill (`validate` first; the meta-skill is refused;
  stamp `last_verified`/`source_event`).
- `reverify` (Fixed barrier) re-runs the eval on just the affected task. The
  improvement is **integrated only if the score now crosses the gate**;
  otherwise `issue{kind: eval_regression}` for human review. Bounded by the
  plan's max attempts + `admit` — **never an unbounded self-edit loop.**

This closes the loop: **eval → diagnosis → improve doc/skill → re-eval →
confirm-or-issue.** "Optional" is literal — G1 distill runs source-driven
without any eval input; G4 is the additional, opt-in eval-driven driver.

*Spec-alignment:* the factual tier realizes the roadmap's aspirational
"agent task-success" benchmark (`docs/spec/roadmap.md:52-53`, `:220`) — today
only retrieval is scored and nothing consumes the report. The roadmap wording is
reconciled alongside this delivery.

## 6. Architecture — reuse vs. new

**Reused unchanged:** the reducer's phase sequencing / `over` fan-out / barrier
tally; `admit` loop controls; `quota`; the packager + `WORKFLOW_STEP_TOOLS`; the
ledger; the reconciler; workflow-aware recovery; the signed capture webhook; the
`neighbours`/`is_cited`/`list_instances`/`search` read surface.

**New / extended:**

| Piece | Where | For |
|---|---|---|
| `writes: existing` + `target_field` + `RunState.targets` | `runner-workflow/src/{spec,step,reduce}.rs`, `runner-core/src/workflow.rs` | G1, G4 |
| lint tick + `ESCUREL_RUNNER_LINT_INTERVAL` | `escurel-runner/src/main.rs`, `runner-core/src/config.rs` | G2 |
| `issue`, `distill-*`, `weave`, `index`, `eval-result` skills + `distill`/`lint`/`eval`/`improve` plans | `runner-workflow/src/corpus.rs` | G1–G4 |
| feature-gated `eval-result` emit path | `crates/escurel-eval/` | G4 |

## 7. Invariants held

- **Fail-closed ACL.** Every operation runs on `Role::Agent` /
  `WORKFLOW_STEP_TOOLS`; **never** `run_stored_query` / admin SQL. Issues, woven
  pages, and improved skills carry normal owner frontmatter; skill edits
  `validate` first and the meta-skill is refused.
- **Determinism / idempotency.** All fan-out is the pure reducer —
  content-addressed step ids, no wall-clock/rand. The lint tick's clock read is
  I/O (like the poller), not reducer logic; freshness timestamps are data
  written by harness runs.
- **Derivability.** Issues, weave records, woven pages, the curated index, and
  eval-results are all ordinary instances under `markdown/` — they survive
  rebuild/audit and keep `AuditDrift` clean.
- **Gateway automation-free; no new ingress.** The only inbound is
  `capture_event` on the existing surface — including the lint tick, where the
  *runner* (not the gateway) emits to itself.
- **Opt-in.** No `ensure_*` at boot for any of this; markdown-only tenants are
  byte-for-byte unaffected.

## 8. Delivery sketch (phased, mirrors the runner epic's DoD)

Each step is one incremental PR with a no-mock integration test as the merge
gate (real `EscurelProcess` + real DuckDB + real `/mcp` + `escurel-echo-harness`).

1. **Docs** — this concept + the Step-0 note. *(this PR)*
2. **G1a reducer extension** — `writes: existing` / `target_field` /
   `RunState.targets` / `source_event` completion. *Test:* reducer unit tests for
   `Existing`-phase planning + completion; a `New`-phase regression proving
   deep-research is unchanged.
3. **G1b distill corpus** — `distill_corpus()` + `distill`/`distill-claim`/
   `weave`/`distill-report`. *Test:* one source updates ≥2 existing entity pages;
   the document/PDF variant reads chunks into claims.
4. **G2a `issue` + lint corpus** — the `issue` kind + `lint_corpus()`. *Test:*
   seeded contradiction/orphan/stale → the right `issue` instances, corpus
   unchanged.
5. **G2b lint tick** — the runner schedule tick + config. *Test:* per-window id
   stable (one run/window); interval disabled emits nothing.
6. **G3 freshness + curation** — freshness stamping + `curation_corpus()` +
   `index`. *Test:* stale-threshold flagging; index regenerates + stays derivable.
7. **G4 eval loop** — `eval_corpus()` (`eval` = score → apply). *Test:* eval
   improves a failing doc/skill and re-eval passes; a broken fix raises an
   `eval_regression` issue.

Throughout: existing tests stay green; a markdown-only tenant (no corpus/eval
seeded) is untouched.

## 8a. As-built notes (where the implementation refined the concept)

- **`eval` and `improve` are one workflow, not two.** The reducer scopes an
  `over` phase to its run's own instances (the `<run_slug>-` page-id prefix), so
  a separate `improve` run cannot fan out over an `eval` run's `eval-result`s.
  The shipped `eval` plan therefore runs `score` → `apply` (the durable-target
  weave) in a single run. The reverify + `eval_regression` guard is a re-run of
  `eval`: a task that still fails on an already-improved page (one carrying
  `source_event`) raises the issue.
- **The driver loads externally-supplied `over` skills.** `build_run_state` now
  loads run-scoped instances of every skill a phase *reads* (`over`), not only
  those it *produces*, so a leading `over` phase over an external input set (e.g.
  `eval`'s `eval-task`s) sees a populated upstream instead of vacuously
  completing. Deep-research is unaffected (its `over` skills are all produced
  upstream).
- **Structural lint / eval, semantic tier deferred.** The shipped
  echo-harness detectors are deterministic and structural (orphan/stale/
  contradiction-by-fact-key; eval-by-expected-substring), which keeps them
  air-gappable and CI-testable. A semantic LLM harness is the richer tier for
  nuanced contradictions and open-ended tasks — same corpus, same events.

## 9. Open questions (non-blocking)

- Should `curate` be its own tick or folded into the lint tick? (Start folded.)
- `target_skill` edits: require a second confirmation phase before mutating a
  skill (the connective tissue) vs. rely on `validate` + reverify? (Start with
  validate + reverify; escalate if traces show skill churn.)
- Contradiction clustering: embedding-neighbourhood vs. recently-touched window
  as the phase's fan-out set. (Start recently-touched; add clustering later.)

## Provenance

Grounded against `origin/main` @ 45b1d4c (2026-07-12) by three parallel code
sweeps (runner/cascade/reducer; agent surface/ACL/Issues; links/frontmatter/
shipping) plus a fourth on `escurel-eval` + skill-editability; every premise in
the task framing was re-checked against source and the corrections recorded in
the Step-0 note.
