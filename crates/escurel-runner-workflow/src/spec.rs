//! The workflow *plan* — the machine-readable orchestration spec parsed
//! from a `kind: workflow` skill's frontmatter (`§3.1`).
//!
//! This is the escurel analogue of deep-research's `export const meta` +
//! phase structure. The reducer (`§3.4`) reads this immutable spec plus the
//! run's instances/log and decides the next batch of steps. Constants
//! deep-research hard-codes in JS (`VOTES_PER_CLAIM`, `MAX_FETCH`,
//! `MAX_VERIFY_CLAIMS`) live here as frontmatter — auditable, editable, and
//! versioned in the KB.
//!
//! Parsing is total and lenient at the read boundary: a malformed phase is
//! skipped rather than panicking, mirroring the rest of the frontmatter
//! surface. The reducer treats an empty phase list as "nothing to do".

use serde_json::Value;

/// The default instance skill each run of a workflow materialises.
pub const DEFAULT_RUN_SKILL: &str = "workflow-run";

/// A parsed workflow plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSkill {
    /// The plan skill id (`id:` frontmatter).
    pub id: String,
    /// The instance skill each invocation produces (`run_skill:`,
    /// default [`DEFAULT_RUN_SKILL`]).
    pub run_skill: String,
    /// The default harness for every phase (`harness:`); a phase may
    /// override it. `None` ⇒ the runner's configured default.
    pub harness: Option<String>,
    /// The ordered phases the reducer sequences through.
    pub phases: Vec<Phase>,
    /// The quorum-verify policy (`verify:` block). Present even when the
    /// plan has no verify phase — the defaults are harmless.
    pub verify: VerifyPolicy,
}

/// One phase of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase {
    /// Phase id (e.g. `scope`, `search`, `verify`).
    pub id: String,
    /// The skill id this phase's runs write instances of. For a
    /// [`WriteMode::Existing`] phase this names the *instruction* skill the
    /// step agent is routed to (how to weave); the instance actually written
    /// is the durable target page.
    pub produces: String,
    /// How this phase fans out.
    pub fan_out: FanOut,
    /// Where this phase writes: a fresh run-scoped instance
    /// ([`WriteMode::New`], the default) or a durable *existing* page named by
    /// a fan-out element's frontmatter field ([`WriteMode::Existing`], the
    /// integrative-distillation weave).
    pub writes: WriteMode,
    /// Optional per-item dedup key (a frontmatter field on the produced
    /// instances, e.g. `norm_url`) applied before the next phase fans out.
    pub dedup_by: Option<String>,
    /// Optional cap on how many produced items advance (`max:`).
    pub max: Option<usize>,
    /// Optional cap on how many ranked targets a barrier opens over
    /// (`max_targets:`, e.g. the top-25 claims by importance × quality).
    pub max_targets: Option<usize>,
    /// Optional per-phase harness override (`harness:` on the phase).
    pub harness: Option<String>,
}

/// Where a phase's steps write their output (`§3.4`, the compile-first-wiki
/// extension — see `docs/contract/compile-first-wiki.md` G1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WriteMode {
    /// A fresh run-scoped instance at the deterministic pre-flagged page id
    /// (`markdown/instances/<produces>/<run_slug>-<phase>-<hash>.md`). Every
    /// pre-existing workflow (deep-research) is `New`.
    #[default]
    New,
    /// A durable, already-existing page. The step's target page id is read
    /// from `target_field` on each fan-out element's frontmatter (e.g. a
    /// `weave` record's `target_page`), so one distill run can weave a fact
    /// into many existing entity/concept pages. Completion is detected by the
    /// harness stamping `source_event` on the target (not by a new instance
    /// landing, since a durable page already exists).
    Existing {
        /// The fan-out element's frontmatter field naming the durable target
        /// page id (e.g. `target_page`, `target_skill`).
        target_field: String,
    },
}

/// How a phase fans out into steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanOut {
    /// A fixed width — `fan_out: 1` (or any integer). One step, no `over`.
    Fixed(u32),
    /// One step per instance of the referenced skill (`fan_out: { over:
    /// research-angle }`), optionally with a per-element `width` (`fan_out:
    /// { over: claim, width: verify.votes_per_claim }`) — that many step
    /// slots per element. `width` is resolved at parse time (a
    /// `verify.votes_per_claim` reference collapses to a number).
    Over { over: String, width: u32 },
}

/// The quorum-verify policy — the single source of truth for both the
/// verify fan-out width and the barrier threshold (`§3.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyPolicy {
    /// Votes gathered per claim (also the verify phase's per-claim width).
    pub votes_per_claim: u32,
    /// Refutations that kill a claim (`≥` this ⇒ dropped).
    pub refutations_required: u32,
}

impl Default for VerifyPolicy {
    fn default() -> Self {
        // deep-research's defaults: 3 votes, 2 refutations kill a claim.
        Self {
            votes_per_claim: 3,
            refutations_required: 2,
        }
    }
}

impl WorkflowSkill {
    /// Parse a workflow plan from a `kind: workflow` skill's frontmatter
    /// JSON. Returns `None` when the frontmatter carries no `phases:` block
    /// (not a workflow plan). A malformed individual phase is skipped.
    #[must_use]
    pub fn parse(fm: &Value) -> Option<Self> {
        let phases_json = fm.get("phases")?.as_array()?;
        let verify = parse_verify(fm.get("verify"));
        let phases = phases_json
            .iter()
            .filter_map(|p| parse_phase(p, &verify))
            .collect();
        Some(Self {
            id: str_field(fm, "id").unwrap_or_default(),
            run_skill: str_field(fm, "run_skill").unwrap_or_else(|| DEFAULT_RUN_SKILL.to_owned()),
            harness: str_field(fm, "harness"),
            phases,
            verify,
        })
    }
}

fn parse_verify(v: Option<&Value>) -> VerifyPolicy {
    let mut policy = VerifyPolicy::default();
    if let Some(obj) = v.and_then(Value::as_object) {
        if let Some(n) = obj.get("votes_per_claim").and_then(Value::as_u64) {
            policy.votes_per_claim = n as u32;
        }
        if let Some(n) = obj.get("refutations_required").and_then(Value::as_u64) {
            policy.refutations_required = n as u32;
        }
    }
    policy
}

fn parse_phase(p: &Value, verify: &VerifyPolicy) -> Option<Phase> {
    let obj = p.as_object()?;
    let id = obj.get("id")?.as_str()?.to_owned();
    let produces = obj.get("produces")?.as_str()?.to_owned();
    let fan_out = parse_fan_out(obj.get("fan_out"), verify);
    let writes = parse_write_mode(obj.get("writes"), obj.get("target_field"));
    Some(Phase {
        id,
        produces,
        fan_out,
        writes,
        dedup_by: obj
            .get("dedup_by")
            .and_then(Value::as_str)
            .map(str::to_owned),
        max: usize_field(obj.get("max")),
        max_targets: usize_field(obj.get("max_targets")),
        harness: obj
            .get("harness")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Parse `fan_out:`. Absent or a bare integer ⇒ [`FanOut::Fixed`]; an object
/// `{ over, width? }` ⇒ [`FanOut::Over`]. A `width` given as the string
/// `"verify.votes_per_claim"` resolves to the policy's value; a bare integer
/// width is taken verbatim; absent ⇒ width 1.
fn parse_fan_out(v: Option<&Value>, verify: &VerifyPolicy) -> FanOut {
    match v {
        None => FanOut::Fixed(1),
        Some(Value::Number(n)) => FanOut::Fixed(n.as_u64().unwrap_or(1) as u32),
        Some(Value::Object(obj)) => {
            let Some(over) = obj.get("over").and_then(Value::as_str) else {
                return FanOut::Fixed(1);
            };
            let width = match obj.get("width") {
                Some(Value::Number(n)) => n.as_u64().unwrap_or(1) as u32,
                Some(Value::String(s)) if s == "verify.votes_per_claim" => verify.votes_per_claim,
                _ => 1,
            };
            FanOut::Over {
                over: over.to_owned(),
                width,
            }
        }
        Some(_) => FanOut::Fixed(1),
    }
}

/// Parse `writes:`. Absent or anything but the string `existing` ⇒
/// [`WriteMode::New`] (the default — every pre-existing workflow). `writes:
/// existing` ⇒ [`WriteMode::Existing`] reading the target page id from
/// `target_field` (defaulting to `target_page` when the key is omitted, so a
/// bare `writes: existing` is still usable).
fn parse_write_mode(writes: Option<&Value>, target_field: Option<&Value>) -> WriteMode {
    match writes.and_then(Value::as_str) {
        Some("existing") => WriteMode::Existing {
            target_field: target_field
                .and_then(Value::as_str)
                .unwrap_or("target_page")
                .to_owned(),
        },
        _ => WriteMode::New,
    }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn usize_field(v: Option<&Value>) -> Option<usize> {
    v.and_then(Value::as_u64).map(|n| n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deep_research_fm() -> Value {
        json!({
            "type": "skill",
            "id": "deep-research",
            "run_skill": "workflow-run",
            "harness": "claude",
            "phases": [
                { "id": "scope", "produces": "research-angle", "fan_out": 1 },
                { "id": "search", "produces": "source", "fan_out": { "over": "research-angle" } },
                { "id": "fetch", "produces": "source", "dedup_by": "norm_url", "max": 15 },
                { "id": "extract", "produces": "claims", "fan_out": { "over": "source" } },
                { "id": "verify", "produces": "verify-vote",
                  "fan_out": { "over": "claim", "width": "verify.votes_per_claim" },
                  "max_targets": 25 },
                { "id": "synthesize", "produces": "research-report", "fan_out": 1 }
            ],
            "verify": { "votes_per_claim": 3, "refutations_required": 2 }
        })
    }

    #[test]
    fn parses_deep_research_plan() {
        let spec = WorkflowSkill::parse(&deep_research_fm()).expect("workflow plan");
        assert_eq!(spec.id, "deep-research");
        assert_eq!(spec.run_skill, "workflow-run");
        assert_eq!(spec.harness.as_deref(), Some("claude"));
        assert_eq!(spec.phases.len(), 6);
        assert_eq!(spec.verify.votes_per_claim, 3);
        assert_eq!(spec.verify.refutations_required, 2);
    }

    #[test]
    fn fan_out_variants_parse() {
        let spec = WorkflowSkill::parse(&deep_research_fm()).unwrap();
        assert_eq!(spec.phases[0].fan_out, FanOut::Fixed(1)); // scope
        assert_eq!(
            spec.phases[1].fan_out, // search
            FanOut::Over {
                over: "research-angle".to_owned(),
                width: 1
            }
        );
        // fetch has no fan_out key ⇒ Fixed(1) with dedup/max metadata.
        assert_eq!(spec.phases[2].fan_out, FanOut::Fixed(1));
        assert_eq!(spec.phases[2].dedup_by.as_deref(), Some("norm_url"));
        assert_eq!(spec.phases[2].max, Some(15));
    }

    #[test]
    fn verify_width_reference_resolves_to_the_policy_value() {
        let spec = WorkflowSkill::parse(&deep_research_fm()).unwrap();
        let verify = spec.phases.iter().find(|p| p.id == "verify").unwrap();
        assert_eq!(
            verify.fan_out,
            FanOut::Over {
                over: "claim".to_owned(),
                width: 3
            },
            "width: verify.votes_per_claim resolves to 3"
        );
        assert_eq!(verify.max_targets, Some(25));
    }

    #[test]
    fn writes_defaults_to_new_and_is_backward_compatible() {
        // deep-research declares no `writes:` — every phase must be New.
        let spec = WorkflowSkill::parse(&deep_research_fm()).unwrap();
        assert!(spec.phases.iter().all(|p| p.writes == WriteMode::New));
    }

    #[test]
    fn writes_existing_parses_with_target_field() {
        let fm = json!({
            "id": "distill",
            "phases": [
                { "id": "weave", "produces": "weave",
                  "fan_out": { "over": "weave-plan" },
                  "writes": "existing", "target_field": "target_page" }
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

    #[test]
    fn writes_existing_defaults_target_field_when_omitted() {
        let fm = json!({
            "id": "distill",
            "phases": [
                { "id": "weave", "produces": "weave",
                  "fan_out": { "over": "weave-plan" }, "writes": "existing" }
            ]
        });
        let spec = WorkflowSkill::parse(&fm).unwrap();
        assert_eq!(
            spec.phases[0].writes,
            WriteMode::Existing {
                target_field: "target_page".to_owned()
            },
            "a bare `writes: existing` defaults target_field to target_page"
        );
    }

    #[test]
    fn non_workflow_frontmatter_is_none() {
        assert_eq!(
            WorkflowSkill::parse(&json!({ "type": "skill", "id": "customer" })),
            None
        );
    }

    #[test]
    fn verify_defaults_when_block_absent() {
        let fm = json!({ "id": "x", "phases": [{ "id": "a", "produces": "b" }] });
        let spec = WorkflowSkill::parse(&fm).unwrap();
        assert_eq!(spec.verify, VerifyPolicy::default());
        assert_eq!(spec.run_skill, DEFAULT_RUN_SKILL);
    }

    #[test]
    fn malformed_phase_is_skipped_not_fatal() {
        let fm = json!({
            "id": "x",
            "phases": [
                { "id": "ok", "produces": "b" },
                { "id": "no-produces" },
                "garbage"
            ]
        });
        let spec = WorkflowSkill::parse(&fm).unwrap();
        assert_eq!(spec.phases.len(), 1);
        assert_eq!(spec.phases[0].id, "ok");
    }
}
