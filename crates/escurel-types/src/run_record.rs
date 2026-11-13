//! The runner's `run-finished` record: ONE definition for the runner that
//! writes it and every reader that folds it (`usage_report`, `list_lineage`,
//! `fetch_blob{run_id}`), so renaming a key breaks the build on both sides
//! instead of silently zeroing a usage group.
//!
//! `run-finished` is an `escurel:run` system event: [`RunFinishedBody`] is its
//! body, [`RunnerProvenance`] the `provenance.runner` block every run event
//! carries.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Token usage and cost of a run (summed over its attempts) or of one harness
/// invocation.
///
/// Counts are what the harness's own accounting reports: for claude each
/// turn's result envelope `usage` summed over the session (input = fresh +
/// cache-read + cache-creation input tokens; the cost is the last envelope's
/// running `total_cost_usd`), for gemini `usageMetadata` summed over the turns, for codex
/// the `turn.completed` usage summed (its `cached_input_tokens` is a subset of
/// `input_tokens`, not added). `cost_usd` only when the harness prices
/// itself (claude's `total_cost_usd`); `model` when it names one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Provider-reported total, including token classes outside input/output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billed_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub model: Option<String>,
}

impl RunUsage {
    /// Fold `other` into `self`: tokens and cost add up (a cost stays `None`
    /// only while nobody reported one), the model is the last one reported.
    pub fn accumulate(&mut self, other: &RunUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.billed_tokens = match (self.billed_tokens, other.billed_tokens) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
        };
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        if other.model.is_some() {
            self.model = other.model.clone();
        }
    }
}

/// What `run-finished.transcript` records about the stored transcript (#822).
/// Informational: a reader fetches the transcript by run id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscriptSummary {
    pub records: u64,
    pub bytes: u64,
    /// `sha256:<hex>` of the stored bytes.
    pub sha256: String,
}

/// The body of a `run-finished` event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunFinishedBody {
    /// `processed` | `failed` | `dead_letter` | `cancelled` | `planned`.
    pub status: String,
    pub attempts: u32,
    pub held: bool,
    pub summary: String,
    pub tool_calls: u32,
    pub produced_instance: Option<String>,
    pub produced_version: Option<String>,
    /// The newest `run-progress` plan, `null` when none was reported.
    pub plan: Value,
    /// Summed over the attempts; `None` when no attempt reported any.
    pub usage: Option<RunUsage>,
    /// `None` when no transcript was stored.
    pub transcript: Option<TranscriptSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The `provenance.runner` block of a run event. The keys readers key on are
/// typed; whatever else the runner records rides along in `rest`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunnerProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_event_id: Option<String>,
    /// The skill the run ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// The agent profile it ran as (#823), when it ran as one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The model the runner chose (a harness may report another in `usage`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_page_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl RunnerProvenance {
    /// The `provenance.runner` block of an event's provenance; empty when absent or malformed.
    #[must_use]
    pub fn of(provenance: &Value) -> Self {
        serde_json::from_value(provenance["runner"].clone()).unwrap_or_default()
    }

    /// The principal a run acted as: `agent:<agent id>` for a run of an agent
    /// profile, else `agent:<skill>` — the subject a run without one is minted as.
    #[must_use]
    pub fn principal(&self) -> Option<String> {
        let named = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
        named(&self.agent)
            .or_else(|| named(&self.skill))
            .map(|s| format!("agent:{s}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_body_round_trips_and_reads_what_older_runners_wrote() {
        let body = RunFinishedBody {
            status: "processed".to_owned(),
            usage: Some(RunUsage {
                input_tokens: 3,
                model: Some("m".to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["usage"]["input_tokens"], 3);
        assert!(v.get("reason").is_none());
        assert_eq!(serde_json::from_value::<RunFinishedBody>(v).unwrap(), body);
        // A minimal body (an older runner, a hand-filed test row) still reads.
        let old: RunFinishedBody =
            serde_json::from_value(json!({ "status": "failed", "usage": null })).unwrap();
        assert_eq!(old.status, "failed");
        assert!(old.usage.is_none());
    }

    #[test]
    fn the_provenance_keeps_unknown_keys_and_names_the_principal() {
        let p = RunnerProvenance::of(&json!({ "runner": {
            "skill": "note", "agent": "scribe", "harness": "echo", "depth": 2 } }));
        assert_eq!(p.principal().as_deref(), Some("agent:scribe"));
        assert_eq!(p.rest["harness"], "echo");
        let bare = RunnerProvenance::of(&json!({ "runner": { "skill": "note" } }));
        assert_eq!(bare.principal().as_deref(), Some("agent:note"));
        assert_eq!(RunnerProvenance::of(&json!({})).principal(), None);
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({ "skill": "note", "agent": "scribe", "harness": "echo", "depth": 2 })
        );
    }
}
