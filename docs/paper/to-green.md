# Getting to zero red

**Status:** plan, 2026-08-03. The draft carries **30** `\todonum{}`
placeholders. This document assigns every one of them an exit, in order, with
its cost and its risk.

(`make` prints 14 because `grep -c` counts *lines*; four table rows carry five
cells each. The Makefile is fixed as part of step 0 — a counter that
under-reports the thing it exists to guard is worse than no counter.)

## The one thing this plan may not do

A red cell may leave the paper through exactly three doors:

1. **MEASURE** — run the harness; the placeholder becomes a real number.
2. **RESCOPE** — the experiment as specified cannot run here; replace it with
   a narrower one that can, and state the narrowing in the paper.
3. **RETIRE** — it cannot run and no honest proxy exists; delete the
   placeholder and keep the finding as prose.

There is no fourth door. No cell is filled with a plausible number, an
estimate, a figure from the predecessor presented as current, or a value
copied from a comparable system. If a measurement disagrees with the paper's
claims, the claims change — that is what the measurement is for.

Every RESCOPE and RETIRE must be *visible in the paper* as a stated
limitation. Silently deleting a table we cannot fill is how a draft becomes
dishonest without anyone deciding to make it so.

## Why "measure everything" is not on the table

Two experiments are blocked on artifacts that do not exist, and no amount of
effort produces them:

- **§6.8, the ADR-0001 gate.** The 460-block and 10,120-block labelled
  corpora the thresholds were declared against are not in this repository and
  not in the research tree. Without the corpus there is no comparison.
- **§6.5, skill growth over three tenants' history.** The production corpora
  live on the deployed instance. There are no tenant `pages/` directories on
  this machine; `examples/` holds fixtures, not history.

So the specification changes regardless. The question is not *whether* to
re-scope but *which* cells to re-scope and which to pay for.

## The assignment

| § | red | cells | door | cost | first? |
|---|---|---|---|---|---|
| 6.7 | substrate matrix (Table 7) | ~~20~~ **done** | MEASURED | — | **1st** |
| 6.8 | ADR-0001 gate | ~~1~~ **done** | RETIRED | — | 2nd |
| 6.3 | retrieval vs flat (Table 4) | 1 | MEASURE | 1.5 d | 4th |
| 6.4 | ablation (Table 5) | 1 | MEASURE | 0.5 d | 5th |
| 6.6 | cascade throughput (Table 6) | 1 | MEASURE | 1 d | 3rd |
| 6.6 | replay convergence | 1 | MEASURE | 1 d | 3rd |
| 6.2 | context cost (Table 3) | 1 | MEASURE, split | 2 d | 6th |
| 6.5 | skill growth (Figure 2) | 1 | RESCOPE | 0.5 d | 7th |
| | | **30** | | **~8.5 d** | |

Ordering is by red-cells-per-day *and* by risk-to-thesis, which happen to
agree: the substrate matrix is both the largest block of red and the
experiment most likely to falsify a claim, so it runs first. There is no
point polishing an evaluation whose central corollary has not been tested.

---

## Step 0 — Fix the counter and the label collision (10 min)

- `make todos` counts occurrences, not lines.
- §6.8 refers to `\todonum{Table 7}` while §6.7's real table is also Table 7
  (`tab:substrates`). One of them is wrong in every build.
- When a table becomes real, its `\todonum{Table N}` reference becomes
  `Table~\ref{tab:...}`. Manual table numbers do not survive an inserted
  float.

## Step 1 — The substrate matrix, §6.7 (20 cells, 2 d)

The highest-value work in the plan: two thirds of the red, and the only
experiment that can falsify the corollary the paper says it underrated.

**Everything it needs already exists.** Real Postgres via testcontainer
(`sql_view_postgres.rs`, `live-postgres` feature, Docker confirmed working);
real REST and real upstream MCP servers started in-process
(`remote_backend_tools.rs`); real PDF extraction (`document_ingestion.rs`).
The work is a fixture tenant carrying five skills over one entity, plus
timing instrumentation — not new infrastructure.

**Pre-registration, written before the run.** The claim is that the agent's
*navigation* sequence is identical across substrates. It is already conceded
that *discovery* is not: live backends are never indexed, so similarity
search does not reach them. If the measurement shows the tool sequence
differs for reasons other than that conceded one, D1′ is weaker than §3.3
claims and §3.3 gets rewritten. Recording this now is what stops a bad result
from being reinterpreted after the fact as a subtlety.

Expected shape, so that a surprise is recognisable: markdown, `sql_view` and
`document` should answer from the local file in single-digit milliseconds;
`openapi` and `mcp` should show the network round trip in p50 and a fatter
p95. If the live p50s come back comparable to local ones, the fixture is
wrong, not the system.

## Step 2 — Retire the ADR-0001 placeholder (10 min)

§6.8 currently says, in prose, that the comparison cannot be reported because
the corpus is gone — and then prints a red placeholder for the table it just
said would not exist. That is incoherent on its face. Delete the placeholder;
keep the finding, which is about process and is one of the better paragraphs
in the paper: *a pre-deployment gate whose fixture is not versioned with the
code is a gate that will still be open when you deploy.*

Add one sentence naming what it would take to reopen it, so the retirement is
a decision rather than an omission.

## Step 3 — Cascade throughput and replay, §6.6 (2 cells, 2 d)

Entirely local, no external services, no model. The echo harness exists
(`escurel-runner-harness/src/echo.rs`), the loop controls have tests
(`loop_controls.rs`), and deterministic step identity is already implemented
(`escurel-runner-workflow/src/key.rs`).

Work: counters for depth, quiescence and per-control stop attribution; a
seeded event-stream generator; a kill-and-replay driver that SIGKILLs at
50 pseudo-random points and diffs final instance state.

**Pre-registration.** We expect at least one divergence class. The paper
already says it would rather report one found and fixed than fifty clean
runs; if all fifty come back clean, the kill points are probably not landing
inside the write window, and the harness needs checking before the result is
believed.

## Step 4 — Retrieval against flat, §6.3 and §6.4 (2 cells, 2 d)

`escurel-eval` runs four of the five configurations today. Two pieces are
missing:

- **A `flat` config** — same corpus bytes, same chunk boundaries, same
  embedder, no skill typing, no filter, no typed expansion, with a `k` sweep
  so the baseline is tuned rather than a straw man.
- **A dataset.** No `datasets/` directory exists. SciFact (5,183 docs, 300
  test queries) downloads from the Hub in BEIR layout. Budget 30–90 minutes
  for the first CPU embed of the corpus with `bge-base-en-v1.5`; the index
  persists under `<dataset>/.eval/`, so the ablation in §6.4 reuses it and
  costs almost nothing after the first run.

The 2×2 ablation is the same harness with two flags, which is why it is
half a day and not two.

## Step 5 — Context cost, §6.2 (1 cell, 2 d) — and the scale trap

This is the headline, and it hides a constraint worth naming before anyone
starts: **the three rows of Table 3 do not cost the same to measure.**

- **Tier-1 tokens at cold start** is deterministic. It counts the
  `(id, description)` pairs `list_skills` returns. It needs no model, no
  embeddings, and no agent — so it can be measured to $10^6$ instances
  cheaply, because instances do not have to be *searchable* to be *counted*.
- **Whole-corpus-in-context** is also just a token count at any scale.
- **Tokens and tool calls to a first correct answer** needs a real model and
  a searchable corpus. Embedding $10^6$ blocks with `bge-base` on CPU is
  days of compute on its own.

So the scale axis splits, and the paper must say so plainly: the invariance
claim is measured across the full $10^2$–$10^6$ range, and agent behaviour is
measured at $10^2$–$10^4$ where embedding is affordable. Pretending otherwise
means either a fabricated row or a month of GPU time.

`GEMINI_API_KEY` is present and the repo already has live-model tests gated
on it (`workflow_end_to_end.rs`, `scripts/gemini-workflow-runner.py`), so the
driver has a precedent to copy rather than invent. Bound the spend up front:
3 arms x 3 scales x 5 runs = 45 sessions, not the 10-run design in `plan.md`,
and report the run count beside the medians.

**The answer key is pre-registered before the first model call.** Graded by
exact match on a named entity or a numeric field, never by a model judge.

## Step 6 — Skill growth, §6.5 (1 cell, 0.5 d) — RESCOPE

The experiment as written wants three production tenants' corpus history.
That data is not on this machine. Two honest options:

- **(a) Narrow it.** Measure skill count against instance count on the
  corpora that *are* reachable — the `examples/` tenants and the versioned
  skill pack's own history — and state exactly what that is: a handful of
  fixtures over ten weeks, which is an illustration and not evidence.
- **(b) Convert it to a limitation.** Move it into §7 as a named open
  question: the falsifier we could not run, and what data would settle it.

**Recommended: (b), plus one sentence of (a).** A figure built from fixtures
would look like evidence at a glance and is not, and this is the *falsifier*
for the paper's central claim — the one place where an unconvincing chart is
worse than an honest absence. Export from the production instance later turns
it back into a real figure.

---

## What the paper looks like afterwards

Zero red, and **fewer claims than the current draft promises**. That is the
correct outcome, not a shortfall: today's red is a promissory note, and the
fix is partly paying it and partly withdrawing the notes that cannot be
honoured. Specifically:

- §6.7 becomes the paper's strongest section: 20 measured cells across five
  real substrates, or a rewritten §3.3 if the corollary does not survive.
- §6.2's headline splits into a strong deterministic result across six orders
  of magnitude and a smaller agent-behaviour result across three.
- §6.5 stops being a figure and becomes an admission.
- §6.8 stops being a placeholder and becomes a process finding.

The abstract must be re-scoped last, after the numbers exist. Writing it
first is how a paper ends up claiming what it wishes it had measured.

## Decisions taken (2026-08-03)

1. **LLM spend: full.** 3 arms x 3 scales x 10 runs = 90 sessions for §6.2.
2. **§6.5: RESCOPE to a limitation** (option b). No production export was
   available, and a growth curve drawn from `examples/` fixtures would look
   like evidence for the paper's own falsifier without being any.

## What step 1 actually found (2026-08-03)

The harness is `crates/escurel-server/tests/substrate_matrix.rs`, behind
`--features live-substrates`. Real Postgres container, the repo's own
`report.pdf` through `/ingest`, an axum CRM, a JSON-RPC MCP server.

- **The corollary survived.** All fifteen navigation steps succeed. The test
  asserts this rather than only recording it.
- **The pre-registration earned its keep twice.**
  - The first run reported `search_finds: true` for the live proxies -- the
    opposite of the truth, and an artefact of the fixture's zero-vector
    embedder, under which every vector is identical and `search` returns the
    whole corpus. Discovery is now read from declared capabilities instead of
    probed. Had it been reported, it would have been a fabricated result
    arrived at honestly.
  - The latency prediction was wrong, and the pre-registration said what to
    do about it: **the SQL view is the slowest substrate** (9.4 ms p50) while
    the live proxies come in at ~6.6 ms, because the proxies are loopback and
    in-process. Their figures are a floor that excludes real network RTT. The
    paper says so, and §7 now says that materialising a source is not
    automatically the fast choice.

## Decisions needed

1. **LLM spend for step 5.** 45 sessions at the bounded design above, or the
   full 10-run design? The bounded one is the default.
2. **Production corpus export for §6.5.** If a tenant export can be made
   available, option (a) becomes a real figure and this stops being a
   limitation. Without it, (b) stands.
3. **Order.** Steps 0–2 are free or nearly so and are strictly improvements;
   they can land before any of the above is decided.
