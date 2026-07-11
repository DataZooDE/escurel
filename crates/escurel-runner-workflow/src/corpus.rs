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

#[cfg(test)]
mod tests {
    use super::*;

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
}
