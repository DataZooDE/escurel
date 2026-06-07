//! The internal `Trigger` — the runner's normalised unit of work.
//!
//! Lifecycle step 3 of
//! [`docs/contract/agent-orchestration.md`](https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md)
//! says the webhook listener *normalises* an incoming
//! [`escurel_types::Event`] into a `Trigger { tenant, event_id,
//! label_skill, instance_page_id?, lineage }`, then enqueues it. This
//! module owns that type and the normalisation; the bounded dispatch
//! queue (#148) and the loop-control gate (#150) consume it later.

use escurel_types::Event;

/// Cascade lineage carried alongside a [`Trigger`].
///
/// The full cascade carrier (`parent_event_id`, `parent_run_id`,
/// `lineage_path`, `cause`, …) is fleshed out with the cascade emitter
/// (#156). For a webhook-origin trigger the lineage is its own root:
/// `root_event_id` is the triggering event and `depth` is `0`. Keeping
/// a minimal-but-present shape now means downstream work-items extend
/// it rather than introduce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lineage {
    /// The event at the root of this cascade. For a webhook-origin
    /// trigger this equals the trigger's own `event_id`.
    pub root_event_id: String,
    /// Cascade depth. `0` for a root (webhook-origin) trigger.
    pub depth: u32,
}

impl Lineage {
    /// A root lineage for a webhook-origin trigger: the event is its
    /// own root at depth `0`.
    pub fn root(event_id: impl Into<String>) -> Self {
        Self {
            root_event_id: event_id.into(),
            depth: 0,
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
}

impl Trigger {
    /// Normalise an inbound webhook [`Event`] (plus the resolved
    /// `tenant`) into a `Trigger`.
    ///
    /// Field mapping: `event_id`/`label_skill` copy across; the wire's
    /// empty-string `instance_page_id` becomes `None` (an unassigned
    /// inbox event), a non-empty one becomes `Some`; the lineage is a
    /// root at depth 0 keyed on the event's own id.
    pub fn from_event(event: &Event, tenant: impl Into<String>) -> Self {
        let instance_page_id = if event.instance_page_id.is_empty() {
            None
        } else {
            Some(event.instance_page_id.clone())
        };
        Self {
            tenant: tenant.into(),
            event_id: event.event_id.clone(),
            label_skill: event.label_skill.clone(),
            instance_page_id,
            lineage: Lineage::root(event.event_id.clone()),
        }
    }
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
    }
}
