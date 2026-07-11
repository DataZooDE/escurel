//! The internal `Trigger` — the runner's normalised unit of work.
//!
//! Lifecycle step 3 of
//! [`docs/contract/agent-orchestration.md`](https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md)
//! says the webhook listener *normalises* an incoming
//! [`escurel_types::Event`] into a `Trigger { tenant, event_id,
//! label_skill, instance_page_id?, lineage }`, then enqueues it. This
//! module owns that type and the normalisation; the bounded dispatch
//! queue (#148) and the loop-control gate (#150) consume it later.

use escurel_types::{Event, WorkflowProvenance};

/// Cascade lineage carried alongside a [`Trigger`].
///
/// For a webhook-origin trigger the lineage is its own root:
/// `root_event_id` is the triggering event, `depth` is `0`, and the
/// `lineage_path` is a single-element chain of just that event.
///
/// When the runner cascades a confirmed write into a follow-on event
/// (#156) it stamps the next hop's lineage into the emitted event's
/// `provenance.runner` JSON. The next time the runner sees that event —
/// it re-enters the *exact same* poll → trigger → package → harness →
/// reconcile pipeline — [`Trigger::from_event`] reads `provenance.runner`
/// back, so `root_event_id` stays stable across the whole cascade and
/// `depth` reflects how far down the chain this hop is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lineage {
    /// The event at the root of this cascade. For a webhook-origin
    /// trigger this equals the trigger's own `event_id`; it stays
    /// constant across every cascaded hop.
    pub root_event_id: String,
    /// Cascade depth. `0` for a root (webhook-origin) trigger; a
    /// cascaded event carries `parent_depth + 1`.
    pub depth: u32,
    /// The chain of event ids from the root down to (and including) this
    /// hop's own event. A root trigger's path is `[event_id]`; a
    /// cascaded hop appends its own event id to the parent's path.
    pub lineage_path: Vec<String>,
    /// The chain of **instance page ids** each hop in this cascade wrote
    /// (#157). Event ids are always fresh per hop, so they cannot detect a
    /// re-visited *instance* — the instance chain can. Cycle prevention
    /// checks whether the candidate target instance is already in this path.
    /// A root trigger starts it empty (it has touched no instance yet); each
    /// cascade hop appends the instance its confirmed write landed on.
    pub instance_path: Vec<String>,
    /// The OTel trace id shared by **every hop of this cascade lineage**
    /// (#158). The root hop mints it (a 32-hex-char W3C trace id); each
    /// cascaded event carries it forward in `provenance.runner.trace_id`, so a
    /// later hop continues the SAME trace rather than starting a fresh one.
    /// `None` for a legacy webhook-origin event with no trace id yet (the
    /// runner mints one when it starts the root span).
    pub trace_id: Option<String>,
}

impl Lineage {
    /// A root lineage for a webhook-origin trigger: the event is its
    /// own root at depth `0`, with a single-element `lineage_path` and an
    /// empty `instance_path` (it has touched no instance yet).
    pub fn root(event_id: impl Into<String>) -> Self {
        let event_id = event_id.into();
        Self {
            root_event_id: event_id.clone(),
            depth: 0,
            lineage_path: vec![event_id],
            instance_path: Vec::new(),
            trace_id: None,
        }
    }
}

/// The runner's normalised unit of work, produced from an inbound
/// webhook [`Event`] (plus the resolved tenant) and consumed by the
/// dispatch queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// The tenant this event belongs to.
    ///
    /// As of #147 the gateway stamps the authoritative `tenant_id` into
    /// the webhook payload; the listener reads it from there. (The
    /// gateway is single-tenant per indexer, so `indexer.tenant()` is the
    /// source of truth.)
    pub tenant: String,
    /// The triggering event's id.
    pub event_id: String,
    /// The skill whose page body is the agent's instructions.
    pub label_skill: String,
    /// The instance the event is already assigned to, if any. `None`
    /// for an unassigned inbox event (the wire sends an empty string).
    pub instance_page_id: Option<String>,
    /// Cascade lineage; root/depth-0 for a webhook-origin trigger.
    pub lineage: Lineage,
    /// The `provenance.workflow` block when this event is a dynamic-workflow
    /// step (`docs/contract/dynamic-workflows.md`). `None` for every
    /// non-workflow trigger — the dispatch loop routes a `Some` to the
    /// reducer where it otherwise calls `emit_cascade`.
    pub workflow: Option<WorkflowProvenance>,
}

impl Trigger {
    /// Normalise an inbound webhook [`Event`] (plus the resolved
    /// `tenant`) into a `Trigger`.
    ///
    /// Field mapping: `event_id`/`label_skill` copy across; the wire's
    /// empty-string `instance_page_id` becomes `None` (an unassigned
    /// inbox event), a non-empty one becomes `Some`.
    ///
    /// Lineage: a webhook-origin event is its own root at depth 0. A
    /// **cascaded** event carries the runner's lineage in its
    /// `provenance.runner` JSON (stamped by the cascade emitter, #156);
    /// when present it is read back here so `root_event_id` stays stable
    /// across the chain and `depth`/`lineage_path` extend to this hop —
    /// the cascaded event re-enters the exact same pipeline as a deeper
    /// hop, never a fresh root.
    pub fn from_event(event: &Event, tenant: impl Into<String>) -> Self {
        let instance_page_id = if event.instance_page_id.is_empty() {
            None
        } else {
            Some(event.instance_page_id.clone())
        };
        let lineage = lineage_from_provenance(&event.provenance, &event.event_id)
            .unwrap_or_else(|| Lineage::root(event.event_id.clone()));
        let workflow = WorkflowProvenance::from_provenance(&event.provenance);
        Self {
            tenant: tenant.into(),
            event_id: event.event_id.clone(),
            label_skill: event.label_skill.clone(),
            instance_page_id,
            lineage,
            workflow,
        }
    }
}

/// Read this hop's [`Lineage`] back out of a cascaded event's
/// `provenance.runner` object (the carrier the cascade emitter, #156,
/// stamped). The emitted event's `provenance.runner` already describes
/// *this* hop (its `depth`, the `root_event_id`, and a `lineage_path`
/// ending at this event), so we read those fields straight through.
///
/// Returns `None` when the event carries no `provenance.runner` (a
/// webhook-origin event) so the caller falls back to a depth-0 root.
fn lineage_from_provenance(provenance: &serde_json::Value, event_id: &str) -> Option<Lineage> {
    let runner = provenance.get("runner")?.as_object()?;
    let root_event_id = runner.get("root_event_id")?.as_str()?.to_owned();
    let depth = u32::try_from(runner.get("depth")?.as_u64()?).ok()?;
    let lineage_path = runner
        .get("lineage_path")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| vec![event_id.to_owned()]);
    let instance_path = runner
        .get("instance_path")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // The cascade-wide trace id (#158): carried forward so every hop continues
    // the same OTel trace. Absent on a legacy event → the runner mints one.
    let trace_id = runner
        .get("trace_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Some(Lineage {
        root_event_id,
        depth,
        lineage_path,
        instance_path,
        trace_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> Event {
        Event {
            event_id: "01ABCDEF".to_owned(),
            source: "gcal".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: "note".to_owned(),
            title: "x".to_owned(),
            body: "y".to_owned(),
            status: "inbox".to_owned(),
            ..Event::default()
        }
    }

    #[test]
    fn from_event_maps_core_fields() {
        let event = sample_event();
        let trigger = Trigger::from_event(&event, "tenant-a");
        assert_eq!(trigger.tenant, "tenant-a");
        assert_eq!(trigger.event_id, "01ABCDEF");
        assert_eq!(trigger.label_skill, "note");
    }

    #[test]
    fn empty_instance_page_id_normalises_to_none() {
        let event = sample_event();
        assert!(event.instance_page_id.is_empty());
        let trigger = Trigger::from_event(&event, "tenant-a");
        assert_eq!(trigger.instance_page_id, None);
    }

    #[test]
    fn nonempty_instance_page_id_normalises_to_some() {
        let mut event = sample_event();
        event.instance_page_id = "inst-42".to_owned();
        let trigger = Trigger::from_event(&event, "tenant-a");
        assert_eq!(trigger.instance_page_id, Some("inst-42".to_owned()));
    }

    #[test]
    fn webhook_origin_lineage_is_root_at_depth_zero() {
        let event = sample_event();
        let trigger = Trigger::from_event(&event, "tenant-a");
        assert_eq!(trigger.lineage.root_event_id, "01ABCDEF");
        assert_eq!(trigger.lineage.depth, 0);
        assert_eq!(trigger.lineage.lineage_path, vec!["01ABCDEF".to_owned()]);
    }

    #[test]
    fn workflow_step_event_reads_workflow_block_back() {
        // A workflow step event carries `provenance.workflow` describing the
        // step: its run, plan skill, phase, deterministic step id, and (for a
        // barrier step) the barrier + the item it fans out over. The trigger
        // exposes it so the dispatch loop can route to the reducer.
        let mut event = sample_event();
        event.event_id = "01HSTEPKEY".to_owned();
        event.label_skill = "verify-vote".to_owned();
        event.provenance = serde_json::json!({
            "workflow": {
                "run": "markdown/instances/workflow-run/r1.md",
                "wf_skill": "deep-research",
                "phase": "verify",
                "step": "01HSTEPKEY",
                "barrier": "verify",
                "over": "[[claim::c12]]",
            },
            "runner": { "root_event_id": "ROOT0", "depth": 2, "lineage_path": ["ROOT0", "01HSTEPKEY"] },
        });
        let trigger = Trigger::from_event(&event, "acme");
        let wf = trigger.workflow.expect("workflow block present");
        assert_eq!(wf.run, "markdown/instances/workflow-run/r1.md");
        assert_eq!(wf.wf_skill, "deep-research");
        assert_eq!(wf.phase, "verify");
        assert_eq!(wf.barrier, "verify");
        assert_eq!(wf.over, "[[claim::c12]]");
        // The runner lineage still parses alongside the workflow block.
        assert_eq!(trigger.lineage.root_event_id, "ROOT0");
        assert_eq!(trigger.lineage.depth, 2);
    }

    #[test]
    fn non_workflow_event_has_no_workflow_block() {
        let trigger = Trigger::from_event(&sample_event(), "acme");
        assert_eq!(trigger.workflow, None);
    }

    #[test]
    fn cascaded_event_reads_lineage_back_from_provenance() {
        // A cascaded event carries `provenance.runner` describing this
        // hop: depth 1, the stable root, and a path root → this event.
        let mut event = sample_event();
        event.event_id = "HOP1".to_owned();
        event.provenance = serde_json::json!({
            "runner": {
                "root_event_id": "ROOT0",
                "parent_event_id": "ROOT0",
                "depth": 1,
                "lineage_path": ["ROOT0", "HOP1"],
                "instance_path": ["markdown/instances/beta/b1.md"],
            }
        });
        let trigger = Trigger::from_event(&event, "tenant-a");
        // depth increments down the chain; root stays stable.
        assert_eq!(trigger.lineage.depth, 1);
        assert_eq!(trigger.lineage.root_event_id, "ROOT0");
        assert_eq!(
            trigger.lineage.lineage_path,
            vec!["ROOT0".to_owned(), "HOP1".to_owned()]
        );
        // The instance chain is read back so cycle detection can spot a
        // re-visited instance (#157).
        assert_eq!(
            trigger.lineage.instance_path,
            vec!["markdown/instances/beta/b1.md".to_owned()]
        );
    }

    #[test]
    fn non_runner_provenance_is_treated_as_a_root() {
        // Provenance without a `runner` object (e.g. an external
        // integration's metadata) must not be mistaken for a cascade.
        let mut event = sample_event();
        event.provenance = serde_json::json!({ "source_system": "gcal" });
        let trigger = Trigger::from_event(&event, "tenant-a");
        assert_eq!(trigger.lineage.depth, 0);
        assert_eq!(trigger.lineage.root_event_id, "01ABCDEF");
    }
}
