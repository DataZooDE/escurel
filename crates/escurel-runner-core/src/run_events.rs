//! A run's lifecycle as `escurel:run` system events (knowledge-workbench
//! backend P1, BRD FR-R-1/2/4/5).
//!
//! The ledger stays the source of truth for admission and retry; these
//! events are its PROJECTION for the humans watching a run — `run-started`
//! when a harness is about to run, one `run-attempt` per try, and
//! `run-finished` with the ledger's terminal. They are `kind: system`
//! (bookkeeping, never inbox work: the gateway stores them `processed` on
//! the target page and the runner's own gate never dispatches them), and
//! they are **best-effort**: a gateway that refuses them must not fail a
//! run that made progress, so every writer returns the client error for
//! the caller to log and count, never to act on.
//!
//! Event ids are deterministic (`run:<run_id>:started`, `:attempt:<n>`,
//! `:finished`), so re-emission — a retry, recovery after a crash — is
//! idempotent through `capture_event`'s first-writer-wins.

use escurel_client::{CaptureEventRequest, Client, Error, ListEventsRequest};
use serde_json::{Value, json};

/// The label every run lifecycle event carries.
pub const RUN_EVENT_LABEL: &str = "escurel:run";

/// What every run event says about its run (`provenance.runner`).
#[derive(Debug, Clone)]
pub struct RunEventCtx {
    pub run_id: String,
    pub root_event_id: String,
    /// The event this run was triggered by.
    pub trigger_event_id: String,
    /// The run that emitted the trigger, for a cascade hop.
    pub parent_run_id: Option<String>,
    pub depth: u32,
    pub lineage_path: Vec<String>,
    pub trace_id: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub max_attempts: u32,
    /// The page the run is folding into — where the events attach. `None`
    /// for an unassigned trigger; the events then stay unassigned too.
    pub target_page_id: Option<String>,
}

/// The ledger terminal a `run-finished` reports.
#[derive(Debug, Clone)]
pub enum RunFinish {
    /// Landed (`produced` = the confirmed page + version) or converged with
    /// nothing to do (`produced: None`). `held` = the effect is a draft.
    Processed {
        produced: Option<(String, String)>,
        held: bool,
    },
    /// Retriable by an operator re-drive.
    Failed { reason: String },
    /// Terminal for the loop controls or the retry policy.
    DeadLetter { reason: String },
    /// Stopped on request while live (workbench backend P2-3a); `reason` is
    /// the requester's, when they gave one.
    Cancelled { reason: String },
}

/// What one attempt reported.
#[derive(Debug, Clone)]
pub struct AttemptReport {
    pub attempt: u32,
    pub started_at: String,
    pub ended_at: String,
    /// `ok` | `converged` | `failed` | `timeout`.
    pub outcome: &'static str,
    pub error: Option<String>,
}

/// Now, in the space-separated microsecond form DuckDB's `TRY_CAST` reads
/// back exactly (the same one the workflow status writer uses).
#[must_use]
pub fn now_ts() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string()
}

impl RunEventCtx {
    fn provenance(&self, extra: Value) -> Value {
        let mut runner = json!({
            "run_id": self.run_id,
            "root_event_id": self.root_event_id,
            "event_id": self.trigger_event_id,
            "depth": self.depth,
            "lineage_path": self.lineage_path,
            "harness": self.harness,
            "max_attempts": self.max_attempts,
        });
        for (key, value) in [
            (
                "parent_run_id",
                self.parent_run_id.as_deref().map(Value::from),
            ),
            ("trace_id", self.trace_id.as_deref().map(Value::from)),
            ("model", self.model.as_deref().map(Value::from)),
            (
                "target_page_id",
                self.target_page_id.as_deref().map(Value::from),
            ),
        ] {
            if let Some(v) = value {
                runner[key] = v;
            }
        }
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                runner[k] = v.clone();
            }
        }
        json!({ "runner": runner })
    }

    async fn write(
        &self,
        client: &Client,
        suffix: &str,
        title: &str,
        body: Value,
        extra: Value,
    ) -> Result<(), Error> {
        client
            .capture_event(CaptureEventRequest {
                event_id: format!("run:{}:{suffix}", self.run_id),
                at: now_ts(),
                source: "escurel-runner".to_owned(),
                mime: "application/json".to_owned(),
                label_skill: RUN_EVENT_LABEL.to_owned(),
                instance_page_id: self.target_page_id.clone().unwrap_or_default(),
                title: title.to_owned(),
                body: body.to_string(),
                provenance: self.provenance(extra),
                kind: "system".to_owned(),
            })
            .await
            .map(|_| ())
    }

    /// `run-started`: the ledger admitted the run and a harness is about to
    /// run it.
    ///
    /// # Errors
    /// The gateway's refusal, for the caller to log — never to act on.
    pub async fn emit_started(&self, client: &Client) -> Result<(), Error> {
        self.write(
            client,
            "started",
            "run-started",
            json!({}),
            json!({ "attempt": 0 }),
        )
        .await
    }

    /// One `run-attempt` per try, written when the try ends.
    ///
    /// # Errors
    /// As [`Self::emit_started`].
    pub async fn emit_attempt(&self, client: &Client, report: &AttemptReport) -> Result<(), Error> {
        let mut body = json!({
            "attempt": report.attempt,
            "started_at": report.started_at,
            "ended_at": report.ended_at,
            "outcome": report.outcome,
        });
        if let Some(e) = &report.error {
            body["error"] = json!(e);
        }
        self.write(
            client,
            &format!("attempt:{}", report.attempt),
            "run-attempt",
            body,
            json!({ "attempt": report.attempt }),
        )
        .await
    }

    /// `run-finished` with the ledger's terminal, the harness's last summary
    /// and tool-call count, and the run's final plan (the newest
    /// `run-progress` snapshot the agent reported, read back best-effort).
    ///
    /// # Errors
    /// As [`Self::emit_started`].
    #[allow(clippy::too_many_arguments)]
    pub async fn emit_finished(
        &self,
        client: &Client,
        attempts: u32,
        finish: &RunFinish,
        summary: &str,
        tool_calls: u32,
        autonomy: Option<&str>,
    ) -> Result<(), Error> {
        let (status, produced, held, reason) = match finish {
            RunFinish::Processed { produced, held } => ("processed", produced.clone(), *held, None),
            RunFinish::Failed { reason } => ("failed", None, false, Some(reason.clone())),
            RunFinish::DeadLetter { reason } => ("dead_letter", None, false, Some(reason.clone())),
            RunFinish::Cancelled { reason } => ("cancelled", None, false, Some(reason.clone())),
        };
        let plan = self.latest_plan(client).await;
        let mut body = json!({
            "status": status,
            "attempts": attempts,
            "held": held,
            "summary": summary,
            "tool_calls": tool_calls,
            "produced_instance": produced.as_ref().map(|(p, _)| p.clone()),
            "produced_version": produced.as_ref().map(|(_, v)| v.clone()),
            "plan": plan,
        });
        if let Some(r) = reason {
            body["reason"] = json!(r);
        }
        let mut extra = json!({ "attempt": attempts });
        if let Some(a) = autonomy {
            extra["autonomy"] = json!(a);
        }
        self.write(client, "finished", "run-finished", body, extra)
            .await
    }

    /// The newest `run-progress` snapshot's plan, or `null`.
    async fn latest_plan(&self, client: &Client) -> Value {
        let Ok(page) = client
            .list_events(ListEventsRequest {
                run_id: self.run_id.clone(),
                include_system: true,
                ..Default::default()
            })
            .await
        else {
            return Value::Null;
        };
        page.events
            .iter()
            .filter(|e| e.title == "run-progress")
            .next_back()
            .and_then(|e| serde_json::from_str::<Value>(&e.body).ok())
            .map(|b| b["plan"].clone())
            .unwrap_or(Value::Null)
    }
}
