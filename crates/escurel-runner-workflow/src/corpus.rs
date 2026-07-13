//! The `/deep-research` corpus — the flagship dynamic workflow, shipped as
//! seedable markdown (`§8` step 11).
//!
//! [`deep_research_corpus`] returns the `(page_id, markdown)` pairs a tenant
//! needs to run `deep-research`: the `kind: workflow` plan, its five typed
//! produced skills (`research-angle` / `source` / `claims` / `verify-vote` /
//! `research-report`), the `workflow-run` board skill, and the operator-facing
//! `verify-tally` stored query (the inspection view of the barrier — the
//! *decision* path is the agent-safe `list_instances` tally in
//! [`crate::barrier`], never this admin SQL).
//!
//! It is a bundle, not an auto-injection: a deployment seeds it explicitly
//! (via `FixtureBuilder`, the loader, or `update_page`) rather than every
//! tenant receiving it unconditionally. The plan's `phases`/`verify`
//! frontmatter uses inline-flow YAML to sidestep block-indent pitfalls; the
//! per-phase markdown sections are the instructions a harness reads.

/// The `deep-research` workflow plan (`kind: workflow`).
pub const DEEP_RESEARCH_PLAN: &str = "---\n\
type: skill\n\
id: deep-research\n\
description: Fan-out web search, adversarially verify claims, synthesize a cited report. Invoke on an underspecified research question.\n\
backend: {kind: workflow}\n\
harness: claude\n\
run_skill: workflow-run\n\
phases: [\
{id: scope, produces: research-angle, fan_out: 1}, \
{id: search, produces: source, fan_out: {over: research-angle}}, \
{id: fetch, produces: source, dedup_by: norm_url, max: 15}, \
{id: extract, produces: claims, fan_out: {over: source}}, \
{id: verify, produces: verify-vote, fan_out: {over: claim, width: verify.votes_per_claim}, max_targets: 25}, \
{id: synthesize, produces: research-report, fan_out: 1}]\n\
verify: {votes_per_claim: 3, refutations_required: 2}\n\
---\n\
# deep-research\n\n\
Answer a bounded question with a cited, fact-checked report. Each phase's\n\
agent reads *this* section for its framing and the phase's `produces:` skill\n\
for the shape of what it must write.\n\n\
## scope\n\
Decompose the question into 3-6 distinct search angles. Write one\n\
`research-angle` instance capturing the angles.\n\n\
## search\n\
Use your web search to find sources for the angle. Write a `source` instance\n\
per hit (its `url`, `norm_url`, and a short excerpt).\n\n\
## fetch\n\
Fetch the source page and write its readable text into the `source` instance.\n\n\
## extract\n\
Read the source and write a `claims` set: 2-5 claims, each `{id, text, quote,\n\
importance, source_quality}` (importance and source_quality in 0..1).\n\n\
## verify\n\
You are a SKEPTIC. Try to REFUTE the claim `{{claim}}`. Write a `verify-vote`\n\
instance with `verdict: refuted | valid | unverified` and a one-line reason\n\
citing the source. Set `claim`, `vote_index`, and `verdict`.\n\n\
## synthesize\n\
Read the surviving claims and write the `research-report` instance: a cited\n\
answer. Link refuted claims for transparency.\n";

/// One research angle set (the scope phase output).
pub const RESEARCH_ANGLE: &str = "---\n\
type: skill\n\
id: research-angle\n\
description: A set of search angles decomposing a research question.\n\
optional_frontmatter: [angles, workflow_run]\n\
---\n\
# research-angle\n";

/// A fetched web source (search + fetch phases).
pub const SOURCE: &str = "---\n\
type: skill\n\
id: source\n\
description: A fetched web source — url, normalised url, and readable text.\n\
optional_frontmatter: [url, norm_url, excerpt, untrusted, workflow_run]\n\
---\n\
# source\n";

/// A set of scored claims extracted from one source (extract phase).
pub const CLAIMS: &str = "---\n\
type: skill\n\
id: claims\n\
description: 2-5 scored claims extracted from a source (id/text/quote/importance/source_quality).\n\
optional_frontmatter: [claims, source, workflow_run]\n\
---\n\
# claims\n";

/// One skeptic's vote on a claim (verify phase; the barrier's unit).
pub const VERIFY_VOTE: &str = "---\n\
type: skill\n\
id: verify-vote\n\
description: One skeptic's vote on a claim — verdict refuted/valid/unverified at a vote slot.\n\
required_frontmatter: [claim, vote_index, verdict]\n\
optional_frontmatter: [reason, workflow_run]\n\
---\n\
# verify-vote\n";

/// The synthesized, cited deliverable (synthesize phase).
pub const RESEARCH_REPORT: &str = "---\n\
type: skill\n\
id: research-report\n\
description: The cited, fact-checked report answering the research question.\n\
optional_frontmatter: [question, workflow_run]\n\
---\n\
# research-report\n";

/// The run board each invocation materialises.
pub const WORKFLOW_RUN: &str = "---\n\
type: skill\n\
id: workflow-run\n\
description: A dynamic-workflow run board — its per-phase progress and status.\n\
optional_frontmatter: [wf_skill, status]\n\
---\n\
# workflow-run\n";

/// The `verify-tally` stored query — operator inspection of the barrier
/// (admin surface; NOT the decision path). Filters the canonical
/// `pages.frontmatter` JSON column (the `frontmatter_index` table is gone).
pub const VERIFY_TALLY_QUERY: &str = "---\n\
type: instance\n\
skill: query\n\
id: verify-tally\n\
db: relational\n\
params:\n\
  - {name: run, type: text, required: true}\n\
sql: \"SELECT json_extract_string(frontmatter, '$.claim') AS claim, count(DISTINCT json_extract_string(frontmatter, '$.vote_index')) AS votes, count(*) FILTER (WHERE json_extract_string(frontmatter, '$.verdict') = 'refuted') AS refutations FROM pages WHERE page_type = 'instance' AND skill = 'verify-vote' AND json_extract_string(frontmatter, '$.workflow_run') = :run GROUP BY claim\"\n\
---\n\
# verify-tally\n\n\
Operator inspection of a run's verify barrier: votes and refutations per\n\
claim. The barrier *decision* runs on the agent-safe list_instances tally.\n";

/// The `(page_id, markdown)` pairs that make up the `/deep-research` corpus.
/// Skills land under `markdown/skills/<id>.md`; the query is an instance of
/// the built-in `query` skill.
#[must_use]
pub fn deep_research_corpus() -> Vec<(String, &'static str)> {
    vec![
        (
            "markdown/skills/deep-research.md".to_owned(),
            DEEP_RESEARCH_PLAN,
        ),
        (
            "markdown/skills/research-angle.md".to_owned(),
            RESEARCH_ANGLE,
        ),
        ("markdown/skills/source.md".to_owned(), SOURCE),
        ("markdown/skills/claims.md".to_owned(), CLAIMS),
        ("markdown/skills/verify-vote.md".to_owned(), VERIFY_VOTE),
        (
            "markdown/skills/research-report.md".to_owned(),
            RESEARCH_REPORT,
        ),
        ("markdown/skills/workflow-run.md".to_owned(), WORKFLOW_RUN),
        (
            "markdown/instances/query/verify-tally.md".to_owned(),
            VERIFY_TALLY_QUERY,
        ),
    ]
}

// --- G1: the `distill` corpus (integrative distillation) -------------------
//
// A `distill` run weaves one source's claims into the *existing* entity/concept
// pages they touch (compile-first-wiki G1). `extract` reads the source and
// writes `distill-claim`s, each tagged with the durable `target_page` it
// belongs to; `weave` is a `writes: existing` phase that fans out over the
// distinct target pages and merges the claim into each (stamping
// `source_event`); `integrate` records what was woven. One run therefore
// touches many existing pages — the width>1 generalization of `emit_cascade`.

/// The `distill` workflow plan (`kind: workflow`).
pub const DISTILL_PLAN: &str = "---\n\
type: skill\n\
id: distill\n\
description: Weave a new source's claims into the existing entity/concept pages they touch. Invoke on an ingested source.\n\
backend: {kind: workflow}\n\
harness: claude\n\
run_skill: workflow-run\n\
phases: [\
{id: extract, produces: distill-claim, fan_out: 1}, \
{id: weave, produces: weave, fan_out: {over: distill-claim}, writes: existing, target_field: target_page, dedup_by: target_page}, \
{id: integrate, produces: distill-report, fan_out: 1}]\n\
---\n\
# distill\n\n\
Fold a source into the knowledge base by weaving each of its claims into the\n\
existing page it belongs to — breadth, not just a new append.\n\n\
## extract\n\
Read the source and write one `distill-claim` instance per atomic claim. For\n\
each claim, resolve which existing entity/concept page it belongs to (use\n\
`search`/`resolve`) and set `target_page` to that page id; set `action` to\n\
`update` (or `create` when no page exists yet). Keep the claim text and a\n\
short supporting quote.\n\n\
## weave\n\
Merge the claim(s) for `{{target_page}}` into that page: `expand` it, add or\n\
correct the fact in place (never clobber unrelated content), cite the source,\n\
and stamp `source_event` + `last_verified`. Write the page back with\n\
`update_page`.\n\n\
## integrate\n\
Write a `distill-report` naming the source and the pages woven — the run's\n\
audit trail.\n";

/// One atomic claim extracted from the source, tagged with its durable target.
pub const DISTILL_CLAIM: &str = "---\n\
type: skill\n\
id: distill-claim\n\
description: One atomic claim from a source, tagged with the existing page it should be woven into.\n\
required_frontmatter: [target_page]\n\
optional_frontmatter: [claim, quote, action, workflow_run]\n\
---\n\
# distill-claim\n";

/// The weave instruction skill — the `writes: existing` phase routes here; the
/// instance it writes is the durable target page, not a `weave` instance.
pub const WEAVE: &str = "---\n\
type: skill\n\
id: weave\n\
description: Merge a source claim into an existing entity/concept page, citing the source and stamping source_event.\n\
optional_frontmatter: [source_event, last_verified, workflow_run]\n\
---\n\
# weave\n";

/// The distill run's audit summary (integrate phase).
pub const DISTILL_REPORT: &str = "---\n\
type: skill\n\
id: distill-report\n\
description: Summary of a distill run — which existing pages were woven, and from what source.\n\
optional_frontmatter: [source, woven_pages, workflow_run]\n\
---\n\
# distill-report\n";

/// The `(page_id, markdown)` pairs that make up the `distill` corpus. Opt-in:
/// a tenant seeds these to enable integrative distillation; a markdown-only
/// tenant that never seeds them is unaffected.
#[must_use]
pub fn distill_corpus() -> Vec<(String, &'static str)> {
    vec![
        ("markdown/skills/distill.md".to_owned(), DISTILL_PLAN),
        ("markdown/skills/distill-claim.md".to_owned(), DISTILL_CLAIM),
        ("markdown/skills/weave.md".to_owned(), WEAVE),
        (
            "markdown/skills/distill-report.md".to_owned(),
            DISTILL_REPORT,
        ),
        ("markdown/skills/workflow-run.md".to_owned(), WORKFLOW_RUN),
    ]
}

// --- G2: the `lint` corpus (semantic health) ------------------------------
//
// `lint` is a scheduled whole-corpus health pass that flags structural and
// semantic problems as typed `issue` instances — it *proposes*, it never
// rewrites the corpus (compile-first-wiki G2). A `scan` step reads the pages
// named by the run board's `scan_skills` and records an `issue` per finding:
// `orphan` (no inbound links, via `neighbours`), `stale` (`last_verified`
// older than the board's `stale_before`, G3), and `contradiction` (two pages
// asserting different values for the same `fact_key`). The scan runs on the
// agent-safe read surface and writes only `issue` instances.

/// The persisted typed Issue — the stored companion to the ephemeral
/// `validate` issue. An ordinary instance, so it is derivable, ACL'd, and
/// queryable via `list_instances(issue, {frontmatter_key: kind})`.
pub const ISSUE: &str = "---\n\
type: skill\n\
id: issue\n\
description: A recorded semantic-health finding — a contradiction, stale claim, orphan page, or missing cross-reference. Lint proposes; a human or write-privileged agent disposes.\n\
required_frontmatter: [kind, severity, subject_page, message]\n\
optional_frontmatter: [suggestion, detected_at, source_run, status]\n\
---\n\
# issue\n";

/// The `lint` workflow plan (`kind: workflow`). A single `scan` pass over the
/// board's `scan_skills` produces `issue` instances.
pub const LINT_PLAN: &str = "---\n\
type: skill\n\
id: lint\n\
description: Scheduled whole-corpus health pass — flag contradictions, stale claims, orphans, and missing cross-references as issues. Proposes, never rewrites.\n\
backend: {kind: workflow}\n\
harness: claude\n\
run_skill: workflow-run\n\
phases: [{id: scan, produces: issue, fan_out: 1}]\n\
---\n\
# lint\n\n\
Survey the knowledge base for health problems and record each as an `issue`.\n\
Change nothing else — you propose, a reviewer disposes.\n\n\
## scan\n\
For each page under review: flag an `orphan` when nothing links to it\n\
(`neighbours` inbound is empty); a `stale` issue when its `last_verified` is\n\
older than the review threshold; a `missing_xref` when it names an entity that\n\
exists as a page but is not linked (suggest the wikilink); and a\n\
`contradiction` when two pages assert different values for the same fact. Set\n\
`kind`, `severity`, `subject_page`, and `message` on each `issue`; suggest a\n\
fix in `suggestion`. Do not edit the pages under review.\n";

/// The eager per-edit maintenance handler (#246). An out-of-band `update_page`
/// (with `ESCUREL_EMIT_EDIT_EVENTS` on) captures a `page-edited` event labelled
/// to this skill; the runner reacts by re-verifying just the touched page —
/// turning the periodic lint tick's catch into an eager per-edit catch. Steps
/// run on the narrowed `WORKFLOW_STEP_TOOLS` surface (read + update_page; the
/// event surface is denied), so a maintenance write can't itself steer the run.
pub const PAGE_EDITED: &str = "---\n\
type: skill\n\
id: page-edited\n\
description: React to an out-of-band page edit — re-verify the touched page and flag any orphan / stale / contradiction it introduced as issues.\n\
optional_frontmatter: [edit]\n\
---\n\
# page-edited\n\n\
A page was edited outside the runner. Re-read it (`expand`), check it against \
the corpus (`search`/`neighbours`), and if the edit introduced an orphan, a \
stale claim, or a contradiction, record an `issue`. Refresh the page's \
`last_verified` if it still holds. Propose; do not silently rewrite other pages.\n";

/// The `(page_id, markdown)` pairs that make up the `lint` corpus. Opt-in.
/// Includes the `page-edited` maintenance handler (#246) so an eager per-edit
/// event has a valid skill to route to.
#[must_use]
pub fn lint_corpus() -> Vec<(String, &'static str)> {
    vec![
        ("markdown/skills/lint.md".to_owned(), LINT_PLAN),
        ("markdown/skills/issue.md".to_owned(), ISSUE),
        ("markdown/skills/page-edited.md".to_owned(), PAGE_EDITED),
        ("markdown/skills/workflow-run.md".to_owned(), WORKFLOW_RUN),
    ]
}

// --- G3: the `curate` corpus (a map of the territory) ----------------------
//
// `curate` regenerates a single `index` instance — a by-category map of the
// corpus with one-line summaries, the "map of the territory" a bare
// `list_skills` cannot be (compile-first-wiki G3). It is generated then
// agent-curated, and stays fully derivable: re-running `curate` reproduces it
// from `pages/` + events.

/// The curated corpus index — a by-category map of the knowledge base.
pub const INDEX: &str = "---\n\
type: skill\n\
id: index\n\
description: A curated, by-category map of the knowledge base — the map of the territory.\n\
optional_frontmatter: [generated_at, workflow_run]\n\
---\n\
# index\n";

/// The `curate` workflow plan (`kind: workflow`).
pub const CURATE_PLAN: &str = "---\n\
type: skill\n\
id: curate\n\
description: Regenerate the curated by-category index of the knowledge base. Generated then agent-curated; stays derivable.\n\
backend: {kind: workflow}\n\
harness: claude\n\
run_skill: workflow-run\n\
phases: [{id: curate, produces: index, fan_out: 1}]\n\
---\n\
# curate\n\n\
## curate\n\
Read `list_skills` and each skill's instances; write a single `index` instance\n\
grouping the corpus by category (skill), with a one-line summary per entry and\n\
a wikilink to each page. Keep it derivable — it must be reproducible from the\n\
corpus alone.\n";

/// The `(page_id, markdown)` pairs that make up the `curate` corpus. Opt-in.
#[must_use]
pub fn curation_corpus() -> Vec<(String, &'static str)> {
    vec![
        ("markdown/skills/curate.md".to_owned(), CURATE_PLAN),
        ("markdown/skills/index.md".to_owned(), INDEX),
        ("markdown/skills/workflow-run.md".to_owned(), WORKFLOW_RUN),
    ]
}

// --- G4: eval-driven improvement (the reinforcement half) ------------------
//
// `eval` scores how well the KB answers a set of tasks and records each
// failure as an `eval-result` naming the implicated page (a document *or a
// skill*). `improve` reads the failing results and weaves the fix into that
// durable page — reusing the G1 `writes: existing` extension, so it can refine
// skills, the connective tissue — then a re-run of `eval` confirms. Optional:
// distillation runs source-driven without any of this (compile-first-wiki G4).

/// One evaluation task: a question the KB should answer, the page that should
/// answer it, the substring that proves it does, and the fix if it doesn't.
pub const EVAL_TASK: &str = "---\n\
type: skill\n\
id: eval-task\n\
description: A task the knowledge base should satisfy — its implicated page, the expected content, and the fix.\n\
required_frontmatter: [implicated_page, expect]\n\
optional_frontmatter: [fix, question, workflow_run]\n\
---\n\
# eval-task\n";

/// One scored evaluation outcome — a failure names the page to improve.
pub const EVAL_RESULT: &str = "---\n\
type: skill\n\
id: eval-result\n\
description: A scored evaluation outcome — verdict pass/fail, and for a failure the implicated target_page and the fix to weave in.\n\
required_frontmatter: [task, verdict]\n\
optional_frontmatter: [target_page, fix, tier, score, workflow_run]\n\
---\n\
# eval-result\n";

/// The weave instruction skill for `eval`'s apply phase (the `writes: existing`
/// phase routes here; the instance it writes is the durable target page/skill).
pub const IMPROVEMENT: &str = "---\n\
type: skill\n\
id: improvement\n\
description: Merge an eval fix into the implicated document or skill, citing the eval-result and stamping freshness.\n\
optional_frontmatter: [source_event, last_verified, workflow_run]\n\
---\n\
# improvement\n";

/// The `eval` workflow plan (`kind: workflow`): score each task, then in the
/// same run weave the fix for each failure into the implicated page (or skill)
/// — the eval-driven improvement loop. `score` and `apply` share one run so the
/// durable-target `apply` can fan out over `score`'s run-scoped eval-results
/// (cross-run data flow is out of scope for the run-prefix-scoped reducer).
pub const EVAL_PLAN: &str = "---\n\
type: skill\n\
id: eval\n\
description: Score how well the knowledge base answers a task set and, in the same run, weave the fix for each failure into the implicated document or skill. Re-run to confirm.\n\
backend: {kind: workflow}\n\
harness: claude\n\
run_skill: workflow-run\n\
phases: [\
{id: score, produces: eval-result, fan_out: {over: eval-task}}, \
{id: apply, produces: improvement, fan_out: {over: eval-result}, writes: existing, target_field: target_page, dedup_by: target_page}]\n\
---\n\
# eval\n\n\
## score\n\
For the task `{{eval-task}}`: answer it from the KB and check the implicated\n\
page for the expected content. Write an `eval-result` with `verdict: pass` or\n\
`verdict: fail`; on failure set `target_page` and `fix`. A task that fails again\n\
*after* a prior improvement raises an `eval_regression` issue for review —\n\
bounded, never an unbounded self-edit loop.\n\n\
## apply\n\
Merge the `fix` from the failing eval-result into `{{target_page}}` (a document\n\
or a skill — validate first; the meta-skill is off-limits) and stamp\n\
`source_event`/`last_verified`. Re-run `eval` to confirm the fix held.\n";

/// The `(page_id, markdown)` pairs for the `eval` corpus. Opt-in; reuses the
/// `issue` skill (for `eval_regression`).
#[must_use]
pub fn eval_corpus() -> Vec<(String, &'static str)> {
    vec![
        ("markdown/skills/eval.md".to_owned(), EVAL_PLAN),
        ("markdown/skills/eval-task.md".to_owned(), EVAL_TASK),
        ("markdown/skills/eval-result.md".to_owned(), EVAL_RESULT),
        ("markdown/skills/improvement.md".to_owned(), IMPROVEMENT),
        ("markdown/skills/issue.md".to_owned(), ISSUE),
        ("markdown/skills/workflow-run.md".to_owned(), WORKFLOW_RUN),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{WorkflowSkill, WriteMode};

    #[test]
    fn corpus_ships_the_plan_typed_skills_and_the_tally() {
        let ids: Vec<String> = deep_research_corpus().into_iter().map(|(p, _)| p).collect();
        for expected in [
            "markdown/skills/deep-research.md",
            "markdown/skills/research-angle.md",
            "markdown/skills/source.md",
            "markdown/skills/claims.md",
            "markdown/skills/verify-vote.md",
            "markdown/skills/research-report.md",
            "markdown/skills/workflow-run.md",
            "markdown/instances/query/verify-tally.md",
        ] {
            assert!(ids.iter().any(|id| id == expected), "missing {expected}");
        }
    }

    #[test]
    fn eval_corpus_ships_the_plans_and_typed_skills() {
        let ids: Vec<String> = eval_corpus().into_iter().map(|(p, _)| p).collect();
        for expected in [
            "markdown/skills/eval.md",
            "markdown/skills/eval-task.md",
            "markdown/skills/eval-result.md",
            "markdown/skills/issue.md",
        ] {
            assert!(ids.iter().any(|id| id == expected), "missing {expected}");
        }
        // The apply phase is a durable-target phase (can weave into a skill).
        assert!(EVAL_PLAN.contains("writes: existing"));
        assert!(EVAL_PLAN.contains("target_field: target_page"));
    }

    #[test]
    fn curation_corpus_ships_the_plan_and_index_skill() {
        let ids: Vec<String> = curation_corpus().into_iter().map(|(p, _)| p).collect();
        for expected in [
            "markdown/skills/curate.md",
            "markdown/skills/index.md",
            "markdown/skills/workflow-run.md",
        ] {
            assert!(ids.iter().any(|id| id == expected), "missing {expected}");
        }
    }

    #[test]
    fn lint_corpus_ships_the_plan_and_issue_skill() {
        let ids: Vec<String> = lint_corpus().into_iter().map(|(p, _)| p).collect();
        for expected in [
            "markdown/skills/lint.md",
            "markdown/skills/issue.md",
            "markdown/skills/workflow-run.md",
        ] {
            assert!(ids.iter().any(|id| id == expected), "missing {expected}");
        }
        assert!(ISSUE.contains("required_frontmatter: [kind, severity, subject_page, message]"));
    }

    #[test]
    fn distill_corpus_ships_its_skills() {
        let ids: Vec<String> = distill_corpus().into_iter().map(|(p, _)| p).collect();
        for expected in [
            "markdown/skills/distill.md",
            "markdown/skills/distill-claim.md",
            "markdown/skills/weave.md",
            "markdown/skills/distill-report.md",
            "markdown/skills/workflow-run.md",
        ] {
            assert!(ids.iter().any(|id| id == expected), "missing {expected}");
        }
    }

    #[test]
    fn distill_plan_declares_a_durable_weave_phase() {
        // The plan const must declare the `writes: existing` durable-target
        // weave over `target_page`. (The full YAML→parse path is exercised by
        // the reducer spec tests and the real-gateway E2E; the reducer crate
        // has no YAML dependency, so we assert the authored contract here.)
        assert!(DISTILL_PLAN.contains("id: distill"));
        assert!(DISTILL_PLAN.contains("backend: {kind: workflow}"));
        assert!(
            DISTILL_PLAN.contains("writes: existing")
                && DISTILL_PLAN.contains("target_field: target_page"),
            "weave must be a durable-target phase"
        );
        // Equivalent parsed shape (proves parse accepts this phase form).
        let fm = serde_json::json!({
            "id": "distill",
            "phases": [
                { "id": "weave", "produces": "weave",
                  "fan_out": { "over": "distill-claim" },
                  "writes": "existing", "target_field": "target_page",
                  "dedup_by": "target_page" }
            ]
        });
        let spec = WorkflowSkill::parse(&fm).unwrap();
        assert_eq!(
            spec.phases[0].writes,
            WriteMode::Existing {
                target_field: "target_page".to_owned()
            }
        );
    }
}
