//! Scheduled triggers (#819): the runner fires a skill on its own when the
//! skill page declares `schedule:`.
//!
//! Each tick the runner lists the tenant's skills, works out the most recent
//! window each schedule has opened, and captures ONE event for it with a
//! deterministic id (`sched:<skill>:<window-start>`). The gateway's
//! first-writer-wins on event ids is what makes a restart, an overlapping
//! tick or a second runner harmless: they all compute the same id.
//!
//! **No backfill, bounded catch-up.** A runner that was down across several
//! windows fires at most the latest one when it comes back, never the backlog
//! — a schedule says "keep this fresh", not "run N times". And it fires that
//! one only while it is within the catch-up grace
//! (`ESCUREL_RUNNER_SCHEDULE_GRACE`, default 15m): a restart or a deploy
//! that spans a window still catches it, but a daily 07:15 job does not run
//! at 15:00 because the runner booted (or the page was written) then — it
//! waits for tomorrow's window.
//!
//! **Budget.** `schedule_budget: {runs_per_day: N}` caps the triggers whose
//! window starts on one UTC day. A window past the cap is not fired; an
//! `escurel:run` system event `run-skipped` (id `<trigger id>:skipped`) says
//! so instead, so an exhausted budget is visible, not silence.
//!
//! The legacy `ESCUREL_RUNNER_LINT_INTERVAL` feeds this same loop as a
//! synthetic entry ([`ScheduleSource::LintInterval`]) that keeps the lint
//! tick's old ids, so an upgrade does not fire the current window twice.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use escurel_client::{CaptureEventRequest, Client, Error, ListEventsRequest, ListSkillsRequest};
use escurel_types::Skill;
use escurel_types::schedule::{CronSchedule, ScheduleError};
use serde_json::json;

use crate::RUN_EVENT_LABEL;

/// The `source` of a trigger fired from a skill's own `schedule:`.
pub const SCHEDULE_SOURCE: &str = "runner-schedule";
/// The `source` the lint shim keeps from the lint tick it replaced.
const LINT_TICK_SOURCE: &str = "runner-lint-tick";
/// The skill `backend.kind` whose runs live on a `workflow-run` page.
const WORKFLOW_BACKEND: &str = "workflow";
/// Newest-first page size when counting a day's triggers.
const BUDGET_PAGE: u32 = 200;
/// Pages read before the count gives up (and treats the budget as spent).
const BUDGET_MAX_PAGES: usize = 20;

/// When an entry's windows open.
#[derive(Debug, Clone)]
pub enum Cadence {
    /// A skill's `schedule:` cron expression.
    Cron(Box<CronSchedule>),
    /// Fixed epoch-aligned windows of this many seconds (the lint shim).
    Every(u64),
}

/// Where an entry came from — it decides the event id and source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleSource {
    /// A skill page's `schedule:`. Ids `sched:<skill>:<window-start>`.
    Frontmatter,
    /// `ESCUREL_RUNNER_LINT_INTERVAL`. Ids `lint-<tenant>-<window index>`,
    /// unchanged from the lint tick so an upgrade does not double-fire.
    LintInterval { tenant: String },
}

/// One skill the runner fires on a schedule.
#[derive(Debug, Clone)]
pub struct ScheduleEntry {
    pub skill: String,
    pub cadence: Cadence,
    /// `schedule_budget.runs_per_day`; `None` = unbounded.
    pub runs_per_day: Option<u32>,
    /// A workflow skill: its trigger names a `workflow-run` page and the
    /// workflow invocation the driver expects.
    pub workflow: bool,
    pub source: ScheduleSource,
    /// How long after its start a window may still fire.
    pub grace: Duration,
}

impl ScheduleEntry {
    /// The entry a `list_skills` row declares: `None` without `schedule:`,
    /// `Some(Err)` when the expression does not parse.
    #[must_use]
    pub fn from_skill(skill: &Skill, grace: Duration) -> Option<Result<Self, ScheduleError>> {
        let expr = skill.schedule.as_deref()?;
        Some(CronSchedule::parse(expr).map(|cron| Self {
            skill: skill.id.clone(),
            cadence: Cadence::Cron(Box::new(cron)),
            runs_per_day: skill.schedule_budget.map(|b| b.runs_per_day),
            workflow: skill.backend.kind == WORKFLOW_BACKEND,
            source: ScheduleSource::Frontmatter,
            grace,
        }))
    }

    /// The lint shim: `lint` every `interval`. Its grace is the interval, so
    /// every window fires within its own span, as the lint tick did.
    #[must_use]
    pub fn lint_interval(tenant: &str, interval: Duration) -> Self {
        Self {
            skill: "lint".to_owned(),
            cadence: Cadence::Every(interval.as_secs().max(1)),
            runs_per_day: None,
            workflow: true,
            source: ScheduleSource::LintInterval {
                tenant: tenant.to_owned(),
            },
            grace: interval,
        }
    }

    /// The start of the latest window opened at or before `now`.
    #[must_use]
    pub fn latest_window(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match &self.cadence {
            Cadence::Cron(cron) => cron.latest_window_start(now),
            Cadence::Every(secs) => {
                let secs = i64::try_from(*secs).ok()?;
                Utc.timestamp_opt(now.timestamp().div_euclid(secs) * secs, 0)
                    .single()
            }
        }
    }

    /// The window to fire at `now`: the latest one, if it opened no longer
    /// than `grace` ago.
    #[must_use]
    pub fn due_window(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let window = self.latest_window(now)?;
        let age = (now - window).to_std().unwrap_or_default();
        (age <= self.grace).then_some(window)
    }

    fn expr(&self) -> String {
        match &self.cadence {
            Cadence::Cron(cron) => cron.expr().to_owned(),
            Cadence::Every(secs) => format!("every {secs}s"),
        }
    }

    /// The window as a lint tick numbered it: `epoch / interval`.
    fn window_index(&self, window: DateTime<Utc>) -> i64 {
        match &self.cadence {
            Cadence::Every(secs) => window.timestamp() / i64::try_from(*secs).unwrap_or(1),
            Cadence::Cron(_) => window.timestamp(),
        }
    }

    /// The deterministic id of this entry's trigger for `window`.
    #[must_use]
    pub fn event_id(&self, window: DateTime<Utc>) -> String {
        match &self.source {
            ScheduleSource::Frontmatter => format!("{}{}", self.id_prefix(), rfc3339(window)),
            ScheduleSource::LintInterval { tenant } => {
                format!("lint-{tenant}-{}", self.window_index(window))
            }
        }
    }

    fn id_prefix(&self) -> String {
        format!("sched:{}:", self.skill)
    }

    /// The page a trigger is bound to: a workflow skill's run page, named per
    /// window like the lint tick's; `None` (unassigned — the harness picks)
    /// for any other skill.
    #[must_use]
    pub fn instance_page_id(&self, window: DateTime<Utc>) -> Option<String> {
        if !self.workflow {
            return None;
        }
        let run = escurel_runner_workflow::DEFAULT_RUN_SKILL;
        let slug = match &self.source {
            ScheduleSource::LintInterval { .. } => self.window_index(window).to_string(),
            ScheduleSource::Frontmatter => window.format("%Y%m%dT%H%M%SZ").to_string(),
        };
        Some(format!("markdown/instances/{run}/{}-{slug}.md", self.skill))
    }

    /// The trigger `capture_event` for `window`.
    #[must_use]
    pub fn trigger(&self, window: DateTime<Utc>) -> CaptureEventRequest {
        let page = self.instance_page_id(window);
        let mut provenance = json!({
            "schedule": { "expr": self.expr(), "window_start": rfc3339(window) }
        });
        if let Some(run) = &page {
            provenance["workflow"] =
                json!({ "run": run, "wf_skill": self.skill, "phase": "invoke" });
        }
        let source = match self.source {
            ScheduleSource::Frontmatter => SCHEDULE_SOURCE,
            ScheduleSource::LintInterval { .. } => LINT_TICK_SOURCE,
        };
        CaptureEventRequest {
            event_id: self.event_id(window),
            source: source.to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: self.skill.clone(),
            instance_page_id: page.unwrap_or_default(),
            title: format!("scheduled {}", self.skill),
            body: format!("Scheduled run of `{}` for {}.", self.skill, rfc3339(window)),
            provenance,
            ..Default::default()
        }
    }

    /// The `run-skipped` record filed in place of a trigger past the budget.
    #[must_use]
    pub fn skipped(&self, window: DateTime<Utc>, runs_per_day: u32) -> CaptureEventRequest {
        let trigger = self.event_id(window);
        CaptureEventRequest {
            event_id: format!("{trigger}:skipped"),
            source: SCHEDULE_SOURCE.to_owned(),
            mime: "application/json".to_owned(),
            label_skill: RUN_EVENT_LABEL.to_owned(),
            title: "run-skipped".to_owned(),
            body: json!({
                "reason": "budget",
                "runs_per_day": runs_per_day,
                "skill": self.skill,
                "trigger_event_id": trigger,
                "window_start": rfc3339(window),
            })
            .to_string(),
            provenance: json!({
                "schedule": { "expr": self.expr(), "window_start": rfc3339(window) }
            }),
            kind: "system".to_owned(),
            ..Default::default()
        }
    }

    /// The window start a trigger id of this entry encodes, if it is one.
    fn window_of(&self, event_id: &str) -> Option<DateTime<Utc>> {
        let rest = event_id.strip_prefix(&self.id_prefix())?;
        DateTime::parse_from_rfc3339(rest)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// What one tick did for one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    /// The trigger for this window is captured (or already was).
    Fired(String),
    /// The budget was spent; the `run-skipped` record is captured.
    Skipped(String),
}

/// The schedule loop's memory: which window each entry last handled, so a
/// tick inside a window it already fired costs one `list_skills`, nothing
/// more. Lost on restart, which is safe — the ids collapse the repeat.
#[derive(Debug)]
pub struct Scheduler {
    /// The catch-up grace for frontmatter schedules.
    grace: Duration,
    handled: HashMap<(String, bool), DateTime<Utc>>,
    warned: HashSet<String>,
}

impl Scheduler {
    #[must_use]
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            handled: HashMap::new(),
            warned: HashSet::new(),
        }
    }

    /// The entries the skill catalogue declares, plus `extra` (the lint
    /// shim). A skill whose `schedule:` does not parse is logged once and
    /// left out; a catalogue read that fails leaves just `extra`.
    pub async fn entries(
        &mut self,
        client: &Client,
        extra: &[ScheduleEntry],
    ) -> Vec<ScheduleEntry> {
        let mut out = extra.to_vec();
        match client.list_skills(ListSkillsRequest::default()).await {
            Ok(resp) => {
                for skill in &resp.skills {
                    match ScheduleEntry::from_skill(skill, self.grace) {
                        Some(Ok(entry)) => out.push(entry),
                        Some(Err(e)) => {
                            if self.warned.insert(format!("{}:{e}", skill.id)) {
                                tracing::warn!(target: "escurel_runner", skill = %skill.id, error = %e, "schedule: unreadable `schedule:`; not fired");
                            }
                        }
                        None => {}
                    }
                }
            }
            Err(e) => {
                tracing::warn!(target: "escurel_runner", error = %e, "schedule: list_skills failed; only configured schedules fire this tick");
            }
        }
        out
    }

    /// Handle every entry's latest window at `now`. Errors are logged and
    /// retried next tick (the window stays unhandled).
    pub async fn tick(
        &mut self,
        client: &Client,
        entries: &[ScheduleEntry],
        now: DateTime<Utc>,
    ) -> Vec<TickOutcome> {
        let mut out = Vec::new();
        for entry in entries {
            let Some(window) = entry.due_window(now) else {
                continue;
            };
            let key = (
                entry.skill.clone(),
                entry.source == ScheduleSource::Frontmatter,
            );
            if self.handled.get(&key) == Some(&window) {
                continue;
            }
            match fire(client, entry, window).await {
                Ok(outcome) => {
                    tracing::info!(target: "escurel_runner", skill = %entry.skill, outcome = ?outcome, "schedule: window handled");
                    self.handled.insert(key, window);
                    out.push(outcome);
                }
                Err(e) => {
                    tracing::warn!(target: "escurel_runner", skill = %entry.skill, error = %e, "schedule: capture failed; retrying next tick");
                }
            }
        }
        out
    }
}

async fn fire(
    client: &Client,
    entry: &ScheduleEntry,
    window: DateTime<Utc>,
) -> Result<TickOutcome, Error> {
    if let Some(cap) = entry.runs_per_day
        && fired_today(client, entry, window, cap).await? >= cap
    {
        let req = entry.skipped(window, cap);
        let id = req.event_id.clone();
        client.capture_event(req).await?;
        return Ok(TickOutcome::Skipped(id));
    }
    let req = entry.trigger(window);
    let id = req.event_id.clone();
    client.capture_event(req).await?;
    Ok(TickOutcome::Fired(id))
}

/// How many of this entry's OTHER triggers have a window on `window`'s UTC
/// day, counted up to `cap` (enough to decide). The window's own trigger is
/// excluded, so a restart re-handling a window it already fired re-captures
/// it (a no-op) instead of skipping it.
async fn fired_today(
    client: &Client,
    entry: &ScheduleEntry,
    window: DateTime<Utc>,
    cap: u32,
) -> Result<u32, Error> {
    let own = entry.event_id(window);
    let day = window.date_naive();
    let mut count = 0;
    let mut cursor = String::new();
    for _ in 0..BUDGET_MAX_PAGES {
        let page = client
            .list_events(ListEventsRequest {
                label_skill: entry.skill.clone(),
                newest_first: true,
                limit: BUDGET_PAGE,
                cursor: cursor.clone(),
                ..Default::default()
            })
            .await?;
        for event in &page.events {
            let Some(w) = entry.window_of(&event.event_id) else {
                continue;
            };
            if event.event_id != own && w.date_naive() == day {
                count += 1;
                if count >= cap {
                    return Ok(count);
                }
            }
        }
        match page.next_cursor {
            Some(next) if !next.is_empty() => cursor = next,
            _ => return Ok(count),
        }
    }
    // A day with more triggers than we read is a day whose budget is spent.
    Ok(cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use escurel_types::{ScheduleBudget, SkillBackend};
    use serde_json::Value;

    const GRACE: Duration = Duration::from_secs(15 * 60);

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn skill(id: &str, schedule: &str, backend: &str) -> Skill {
        Skill {
            id: id.to_owned(),
            schedule: Some(schedule.to_owned()),
            schedule_budget: Some(ScheduleBudget { runs_per_day: 3 }),
            backend: SkillBackend {
                kind: backend.to_owned(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_frontmatter_schedule_ids_its_window_by_start_and_skill() {
        let e = ScheduleEntry::from_skill(&skill("digest", "0 7 * * *", "markdown"), GRACE)
            .unwrap()
            .unwrap();
        assert_eq!(e.runs_per_day, Some(3));
        let w = e.latest_window(utc("2026-11-13T09:30:00Z")).unwrap();
        assert_eq!(w, utc("2026-11-13T07:00:00Z"));
        assert_eq!(e.event_id(w), "sched:digest:2026-11-13T07:00:00Z");
        assert_eq!(e.window_of(&e.event_id(w)), Some(w));
        assert_eq!(e.window_of("sched:other:2026-11-13T07:00:00Z"), None);
        // Not a workflow skill: unassigned, the harness picks the page.
        assert_eq!(e.instance_page_id(w), None);
        let t = e.trigger(w);
        assert_eq!(t.label_skill, "digest");
        assert_eq!(t.source, SCHEDULE_SOURCE);
        assert!(t.instance_page_id.is_empty());
        assert_eq!(
            t.provenance["schedule"]["window_start"],
            "2026-11-13T07:00:00Z"
        );
        assert!(t.provenance.get("workflow").is_none());
    }

    #[test]
    fn a_window_fires_only_within_the_grace() {
        let e = ScheduleEntry::from_skill(&skill("digest", "15 7 * * *", "markdown"), GRACE)
            .unwrap()
            .unwrap();
        let window = utc("2026-11-13T07:15:00Z");
        // At the window, and at the edge of the grace: due.
        assert_eq!(e.due_window(window), Some(window));
        assert_eq!(e.due_window(utc("2026-11-13T07:30:00Z")), Some(window));
        // Past the grace: the 07:15 job does not run late at 07:31 or 15:00.
        assert_eq!(e.due_window(utc("2026-11-13T07:30:01Z")), None);
        assert_eq!(e.due_window(utc("2026-11-13T15:00:00Z")), None);
        // The lint shim's grace is its interval: every window is due all along.
        let lint = ScheduleEntry::lint_interval("acme", Duration::from_secs(1800));
        assert!(lint.due_window(utc("2026-11-13T09:59:59Z")).is_some());
    }

    #[test]
    fn a_workflow_skill_gets_a_run_page_per_window() {
        let e = ScheduleEntry::from_skill(&skill("sweep", "*/2 * * * * *", "workflow"), GRACE)
            .unwrap()
            .unwrap();
        let w = e.latest_window(utc("2026-11-13T09:30:05Z")).unwrap();
        let page = "markdown/instances/workflow-run/sweep-20261113T093004Z.md";
        assert_eq!(e.instance_page_id(w).as_deref(), Some(page));
        let t = e.trigger(w);
        assert_eq!(t.instance_page_id, page);
        assert_eq!(
            t.provenance["workflow"],
            json!({ "run": page, "wf_skill": "sweep", "phase": "invoke" })
        );
    }

    #[test]
    fn the_lint_shim_keeps_the_lint_ticks_ids_and_pages() {
        let e = ScheduleEntry::lint_interval("acme", Duration::from_secs(1800));
        let w = e.latest_window(utc("2026-11-13T09:40:00Z")).unwrap();
        assert_eq!(w, utc("2026-11-13T09:30:00Z"));
        let index = w.timestamp() / 1800;
        assert_eq!(e.event_id(w), format!("lint-acme-{index}"));
        assert_eq!(
            e.instance_page_id(w).unwrap(),
            format!("markdown/instances/workflow-run/lint-{index}.md")
        );
        let t = e.trigger(w);
        assert_eq!(t.source, "runner-lint-tick");
        assert_eq!(t.label_skill, "lint");
        assert_eq!(t.provenance["workflow"]["wf_skill"], "lint");
    }

    #[test]
    fn a_skipped_record_is_a_system_run_event_naming_the_budget() {
        let e = ScheduleEntry::from_skill(&skill("digest", "0 7 * * *", "markdown"), GRACE)
            .unwrap()
            .unwrap();
        let w = utc("2026-11-13T07:00:00Z");
        let s = e.skipped(w, 3);
        assert_eq!(s.event_id, "sched:digest:2026-11-13T07:00:00Z:skipped");
        assert_eq!(s.label_skill, RUN_EVENT_LABEL);
        assert_eq!(s.kind, "system");
        assert_eq!(s.title, "run-skipped");
        let body: Value = serde_json::from_str(&s.body).unwrap();
        assert_eq!(body["reason"], "budget");
        assert_eq!(body["runs_per_day"], 3);
        // A skipped id is not mistaken for a trigger when counting.
        assert!(e.window_of(&s.event_id).is_none());
    }

    #[test]
    fn an_unparseable_schedule_is_reported_and_an_absent_one_is_not_an_entry() {
        assert!(matches!(
            ScheduleEntry::from_skill(&skill("bad", "whenever", "markdown"), GRACE),
            Some(Err(_))
        ));
        let mut none = skill("plain", "", "markdown");
        none.schedule = None;
        assert!(ScheduleEntry::from_skill(&none, GRACE).is_none());
    }
}
