//! Usage and cost rollups over `run-finished` (#821).
//!
//! Every run the runner finishes leaves one `escurel:run` system event
//! titled `run-finished` whose body carries the run's terminal `status` and
//! its `usage` summed over the attempts, and whose `provenance.runner` names
//! the skill, model and target page. This module reads those rows back and
//! folds them into per-group totals — a pure fold ([`UsageFold`]) fed by the
//! ordinary event cursor ([`Indexer::usage_report`]), so every events backend
//! answers it without a dialect of its own.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{
    EVENTS_MAX_LIMIT, EventInfo, EventKind, EventListFilter, EventTs, Indexer, IndexerError,
};

/// The label a run's lifecycle events carry.
const RUN_LABEL: &str = "escurel:run";
/// The lifecycle event that carries a run's bill.
const RUN_FINISHED_TITLE: &str = "run-finished";

/// One axis a usage report can group by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageDim {
    /// The skill whose event triggered the run.
    Skill,
    /// The principal the run acted as (see [`run_agent`]).
    Agent,
    /// The page the run folded into.
    Page,
    /// The model the harness reported (else the one the runner chose).
    Model,
    /// The UTC day the run finished on, `YYYY-MM-DD`.
    Day,
}

impl UsageDim {
    /// Every axis, in wire spelling order.
    pub const ALL: [UsageDim; 5] = [
        UsageDim::Skill,
        UsageDim::Agent,
        UsageDim::Page,
        UsageDim::Model,
        UsageDim::Day,
    ];

    /// The wire name: `skill` | `agent` | `page` | `model` | `day`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UsageDim::Skill => "skill",
            UsageDim::Agent => "agent",
            UsageDim::Page => "page",
            UsageDim::Model => "model",
            UsageDim::Day => "day",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == s)
    }
}

/// The principal a run acted as. Today a run's token is always
/// `agent:<skill>`; agent profiles will name their own, and this is the one
/// place that has to learn where to read it from.
#[must_use]
pub fn run_agent(runner: &Value) -> Option<String> {
    runner["skill"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| format!("agent:{s}"))
}

/// One finished run's bill, read from its `run-finished` row.
#[derive(Debug, Clone, PartialEq)]
pub struct RunUsage {
    pub skill: Option<String>,
    pub agent: Option<String>,
    pub page: Option<String>,
    pub model: Option<String>,
    pub day: Option<String>,
    pub status: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `0.0` when the harness priced nothing.
    pub cost_usd: f64,
}

impl RunUsage {
    /// The bill of a `run-finished` row; `None` for any other event.
    #[must_use]
    pub fn from_event(e: &EventInfo) -> Option<Self> {
        if e.label_skill != RUN_LABEL || e.title != RUN_FINISHED_TITLE {
            return None;
        }
        let body: Value = serde_json::from_str(&e.body).unwrap_or(Value::Null);
        let runner = &e.provenance["runner"];
        let usage = &body["usage"];
        let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        Some(Self {
            skill: text(&runner["skill"]),
            agent: run_agent(runner),
            page: text(&runner["target_page_id"]).or_else(|| e.instance_page_id.clone()),
            model: text(&usage["model"]).or_else(|| text(&runner["model"])),
            day: e
                .at
                .as_deref()
                .and_then(|at| at.get(..10))
                .map(str::to_owned),
            status: text(&body["status"]).unwrap_or_else(|| "unknown".to_owned()),
            input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
            cost_usd: usage["cost_usd"]
                .as_f64()
                .filter(|c| c.is_finite() && *c >= 0.0)
                .unwrap_or(0.0),
        })
    }

    fn value(&self, dim: UsageDim) -> Option<&str> {
        match dim {
            UsageDim::Skill => self.skill.as_deref(),
            UsageDim::Agent => self.agent.as_deref(),
            UsageDim::Page => self.page.as_deref(),
            UsageDim::Model => self.model.as_deref(),
            UsageDim::Day => self.day.as_deref(),
        }
    }
}

/// What a set of runs cost.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageTotals {
    pub runs: u64,
    /// Runs per terminal status (`processed`, `failed`, …).
    pub by_status: BTreeMap<String, u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

impl UsageTotals {
    fn add(&mut self, r: &RunUsage) {
        self.runs += 1;
        *self.by_status.entry(r.status.clone()).or_default() += 1;
        self.input_tokens = self.input_tokens.saturating_add(r.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(r.output_tokens);
        self.cost_usd += r.cost_usd;
    }

    /// `{runs, by_status, input_tokens, output_tokens, cost_usd}`. The cost
    /// is rounded to the nano-dollar so a sum of decimal prices reads back
    /// as the decimal a person would write, not its binary neighbour.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "runs": self.runs,
            "by_status": self.by_status,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cost_usd": (self.cost_usd * 1e9).round() / 1e9,
        })
    }
}

/// The fold behind `usage_report`: feed it event rows in any order, read
/// the grouped totals back. Rows that are not `run-finished`, or belong to
/// another skill than the one asked for, are ignored.
#[derive(Debug, Clone)]
pub struct UsageFold {
    group_by: Vec<UsageDim>,
    skill: Option<String>,
    rows: BTreeMap<Vec<Option<String>>, UsageTotals>,
    total: UsageTotals,
}

impl UsageFold {
    /// Group by `group_by` (duplicates dropped, order kept), optionally
    /// counting only runs of `skill`.
    #[must_use]
    pub fn new(group_by: &[UsageDim], skill: Option<String>) -> Self {
        let mut dims = Vec::new();
        for d in group_by {
            if !dims.contains(d) {
                dims.push(*d);
            }
        }
        Self {
            group_by: dims,
            skill: skill.filter(|s| !s.is_empty()),
            rows: BTreeMap::new(),
            total: UsageTotals::default(),
        }
    }

    pub fn add(&mut self, e: &EventInfo) {
        let Some(run) = RunUsage::from_event(e) else {
            return;
        };
        if self.skill.is_some() && run.skill != self.skill {
            return;
        }
        let key = self
            .group_by
            .iter()
            .map(|d| run.value(*d).map(str::to_owned))
            .collect();
        self.rows.entry(key).or_default().add(&run);
        self.total.add(&run);
    }

    #[must_use]
    pub fn total(&self) -> &UsageTotals {
        &self.total
    }

    /// `{group_by, rows: [{group, …totals}], total}`; rows ordered by their
    /// group values.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|(key, totals)| {
                let group: serde_json::Map<String, Value> = self
                    .group_by
                    .iter()
                    .zip(key)
                    .map(|(d, v)| (d.as_str().to_owned(), json!(v)))
                    .collect();
                let mut row = totals.to_json();
                row["group"] = Value::Object(group);
                row
            })
            .collect();
        json!({
            "group_by": self.group_by.iter().map(|d| d.as_str()).collect::<Vec<_>>(),
            "rows": rows,
            "total": self.total.to_json(),
        })
    }
}

impl Indexer {
    /// Fold every `run-finished` whose `at` lies in `[since, until)` into
    /// `fold`, paging through the run label with the ordinary event cursor.
    ///
    /// # Errors
    /// When an event page cannot be read.
    pub async fn usage_report(
        &self,
        since: Option<EventTs>,
        until: Option<EventTs>,
        mut fold: UsageFold,
    ) -> Result<UsageFold, IndexerError> {
        let filter = EventListFilter {
            label_skill: Some(RUN_LABEL.to_owned()),
            kind: Some(EventKind::System),
            since,
            until,
            ..Default::default()
        };
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .list_events_filtered_page(&filter, true, EVENTS_MAX_LIMIT, cursor.as_deref())
                .await?;
            for e in &page.events {
                fold.add(e);
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(fold),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(skill: &str, model: &str, at: &str, status: &str, usage: Value) -> EventInfo {
        EventInfo {
            event_id: format!("run:{skill}-{at}:finished"),
            at: Some(at.to_owned()),
            source: "escurel-runner".to_owned(),
            mime: "application/json".to_owned(),
            label_skill: RUN_LABEL.to_owned(),
            instance_page_id: Some(format!("markdown/instances/{skill}/x.md")),
            status: "processed".to_owned(),
            title: RUN_FINISHED_TITLE.to_owned(),
            body: json!({ "status": status, "usage": usage }).to_string(),
            provenance: json!({ "runner": { "skill": skill, "model": model } }),
            kind: EventKind::System,
            root_event_id: None,
            run_id: None,
        }
    }

    fn usage(i: u64, o: u64, c: f64, model: &str) -> Value {
        json!({ "input_tokens": i, "output_tokens": o, "cost_usd": c, "model": model })
    }

    #[test]
    fn a_run_finished_row_reads_as_its_bill() {
        let e = finished(
            "renewal",
            "chosen",
            "2026-10-01T09:00:00Z",
            "failed",
            usage(10, 2, 0.1, "reported"),
        );
        let r = RunUsage::from_event(&e).expect("a run-finished row");
        assert_eq!(r.skill.as_deref(), Some("renewal"));
        assert_eq!(r.agent.as_deref(), Some("agent:renewal"));
        assert_eq!(r.page.as_deref(), Some("markdown/instances/renewal/x.md"));
        // The harness's own report wins over the runner's choice.
        assert_eq!(r.model.as_deref(), Some("reported"));
        assert_eq!(r.day.as_deref(), Some("2026-10-01"));
        assert_eq!(r.status, "failed");
        assert_eq!((r.input_tokens, r.output_tokens), (10, 2));
    }

    #[test]
    fn a_run_without_usage_still_counts_and_keeps_the_chosen_model() {
        let e = finished(
            "renewal",
            "chosen",
            "2026-10-01T09:00:00Z",
            "processed",
            Value::Null,
        );
        let r = RunUsage::from_event(&e).unwrap();
        assert_eq!(r.model.as_deref(), Some("chosen"));
        assert_eq!((r.input_tokens, r.output_tokens, r.cost_usd), (0, 0, 0.0));
    }

    #[test]
    fn other_lifecycle_rows_are_not_bills() {
        let mut e = finished("s", "m", "2026-10-01T09:00:00Z", "processed", Value::Null);
        e.title = "run-started".to_owned();
        assert!(RunUsage::from_event(&e).is_none());
        e.title = RUN_FINISHED_TITLE.to_owned();
        e.label_skill = "renewal".to_owned();
        assert!(RunUsage::from_event(&e).is_none());
    }

    #[test]
    fn the_fold_groups_sums_and_narrows_by_skill() {
        let rows = [
            finished(
                "a",
                "m1",
                "2026-10-01T01:00:00Z",
                "processed",
                usage(1, 2, 0.25, "m1"),
            ),
            finished(
                "a",
                "m2",
                "2026-10-02T01:00:00Z",
                "failed",
                usage(3, 4, 0.5, "m2"),
            ),
            finished(
                "b",
                "m1",
                "2026-10-02T02:00:00Z",
                "processed",
                usage(5, 6, 1.0, "m1"),
            ),
        ];
        let mut by_skill_day =
            UsageFold::new(&[UsageDim::Skill, UsageDim::Day, UsageDim::Skill], None);
        rows.iter().for_each(|e| by_skill_day.add(e));
        let out = by_skill_day.to_json();
        assert_eq!(out["group_by"], json!(["skill", "day"]));
        assert_eq!(out["rows"].as_array().unwrap().len(), 3);
        assert_eq!(
            out["rows"][0]["group"],
            json!({ "skill": "a", "day": "2026-10-01" })
        );
        assert_eq!(
            out["total"],
            json!({ "runs": 3, "by_status": { "failed": 1, "processed": 2 },
                    "input_tokens": 9, "output_tokens": 12, "cost_usd": 1.75 })
        );

        let mut only_a = UsageFold::new(&[UsageDim::Model], Some("a".to_owned()));
        rows.iter().for_each(|e| only_a.add(e));
        assert_eq!(only_a.total().runs, 2);
        assert_eq!(
            only_a.to_json()["rows"][1]["group"],
            json!({ "model": "m2" })
        );
    }

    #[test]
    fn a_decimal_cost_sum_reads_back_as_the_decimal() {
        let mut fold = UsageFold::new(&[UsageDim::Skill], None);
        for _ in 0..3 {
            fold.add(&finished(
                "a",
                "m",
                "2026-10-01T01:00:00Z",
                "processed",
                usage(0, 0, 0.1, "m"),
            ));
        }
        assert_eq!(fold.to_json()["total"]["cost_usd"], json!(0.3));
    }

    #[test]
    fn dims_parse_from_their_wire_names_only() {
        for d in UsageDim::ALL {
            assert_eq!(UsageDim::parse(d.as_str()), Some(d));
        }
        assert_eq!(UsageDim::parse("colour"), None);
    }
}
