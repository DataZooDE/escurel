//! The bounded, dedup-collapsing dispatch queue (#148).
//!
//! Lifecycle step 3 of
//! [`docs/contract/agent-orchestration.md`](https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md)
//! says the webhook listener and the inbox poller both *converge on one
//! queue*; the contract's "Concurrency / safety" section calls for
//! **at-least-once delivery, effectively-once processing** via dedup. This
//! module is that convergence point: a cloneable producer handle backed by a
//! bounded async channel, fronted by a **bounded seen-set** so a `Trigger`
//! whose `event_id` has already been enqueued is *collapsed* (counted as a
//! duplicate, never enqueued twice) regardless of whether it arrived by
//! webhook or by poll.
//!
//! The bound (`capacity`) gives backpressure: when the channel is full,
//! [`DispatchQueue::enqueue`] returns [`EnqueueOutcome::Full`] rather than
//! blocking — the caller (a `202`-returning webhook handler, or the poller's
//! best-effort loop) drops the trigger and lets the poller re-pull it later.
//! The seen-set is itself bounded (FIFO eviction at `seen_cap`) so a
//! long-running runner does not grow it without limit; an evicted `event_id`
//! can be re-enqueued (the run ledger, #149, is the durable
//! effectively-once authority — the seen-set is only a cheap in-memory
//! collapse for the webhook/poll overlap window).

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::Trigger;

/// Outcome of an [`DispatchQueue::enqueue`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// The trigger's `event_id` had not been seen; it was placed on the
    /// queue and recorded in the seen-set.
    Enqueued,
    /// The trigger's `event_id` was already in the seen-set; the trigger
    /// was collapsed (not enqueued again). This is the webhook/poll
    /// convergence in action.
    Duplicate,
    /// The queue was at capacity (backpressure). The trigger was **not**
    /// enqueued and its `event_id` was **not** recorded, so a later
    /// attempt (e.g. the next poll) can retry it.
    Full,
}

/// Shared dedup state behind the queue: the set of `event_id`s already
/// enqueued, plus a FIFO of insertion order so the set can be bounded.
#[derive(Debug)]
struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl Seen {
    fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Returns `true` if `id` was newly recorded, `false` if it was
    /// already present (a duplicate). On insert, evicts the oldest entry
    /// when over capacity.
    fn record(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        self.set.insert(id.to_owned());
        self.order.push_back(id.to_owned());
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    fn snapshot(&self) -> Vec<String> {
        self.order.iter().cloned().collect()
    }
}

/// The cloneable producer handle. Both the webhook handler and the inbox
/// poller hold a clone and call [`enqueue`](Self::enqueue); dedup is shared
/// across all clones.
#[derive(Debug, Clone)]
pub struct DispatchQueue {
    tx: mpsc::Sender<Trigger>,
    seen: Arc<Mutex<Seen>>,
}

/// The single consumer handle. Held by the dispatch loop (a later work-item
/// drives the harness); here it exists so the queue has a real receiving
/// side and `enqueue` exercises true backpressure.
#[derive(Debug)]
pub struct DispatchConsumer {
    rx: mpsc::Receiver<Trigger>,
}

impl DispatchQueue {
    /// Build a bounded queue with channel capacity `capacity` and a
    /// seen-set bounded at `seen_cap`. Returns the cloneable producer and
    /// the single consumer.
    ///
    /// Both bounds are clamped to at least `1`.
    pub fn new(capacity: usize, seen_cap: usize) -> (Self, DispatchConsumer) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let queue = Self {
            tx,
            seen: Arc::new(Mutex::new(Seen::new(seen_cap))),
        };
        (queue, DispatchConsumer { rx })
    }

    /// Enqueue `trigger`, collapsing duplicates by `event_id`.
    ///
    /// - If the `event_id` is already in the seen-set →
    ///   [`EnqueueOutcome::Duplicate`] (not enqueued).
    /// - Else if the channel has room → record the `event_id`, send it →
    ///   [`EnqueueOutcome::Enqueued`].
    /// - Else (channel full) → [`EnqueueOutcome::Full`]; the `event_id` is
    ///   left unrecorded so a retry can succeed.
    ///
    /// Non-blocking; safe to call from a `202`-returning handler.
    pub fn enqueue(&self, trigger: Trigger) -> EnqueueOutcome {
        // Lock the seen-set across the dedup check + send so two racing
        // callers with the same `event_id` cannot both enqueue.
        let mut seen = self.seen.lock().expect("dispatch seen-set mutex");
        if seen.set.contains(&trigger.event_id) {
            return EnqueueOutcome::Duplicate;
        }
        match self.tx.try_send(trigger.clone()) {
            Ok(()) => {
                seen.record(&trigger.event_id);
                EnqueueOutcome::Enqueued
            }
            Err(mpsc::error::TrySendError::Full(_)) => EnqueueOutcome::Full,
            // The consumer was dropped: treat as full (cannot enqueue).
            Err(mpsc::error::TrySendError::Closed(_)) => EnqueueOutcome::Full,
        }
    }

    /// Snapshot of the seen-set's `event_id`s in insertion order. Backs the
    /// runner's `GET /debug/seen` introspection endpoint.
    pub fn seen_event_ids(&self) -> Vec<String> {
        self.seen
            .lock()
            .expect("dispatch seen-set mutex")
            .snapshot()
    }
}

impl DispatchConsumer {
    /// Await the next trigger, or `None` once every [`DispatchQueue`] clone
    /// has been dropped.
    pub async fn recv(&mut self) -> Option<Trigger> {
        self.rx.recv().await
    }

    /// Non-blocking receive, for tests that want to drain without awaiting.
    pub fn try_recv(&mut self) -> Option<Trigger> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lineage;

    fn trigger(event_id: &str) -> Trigger {
        Trigger {
            tenant: "acme".to_owned(),
            event_id: event_id.to_owned(),
            label_skill: "note".to_owned(),
            instance_page_id: None,
            lineage: Lineage::root(event_id),
        }
    }

    #[test]
    fn same_event_id_twice_is_one_enqueued_one_duplicate() {
        let (q, mut consumer) = DispatchQueue::new(16, 64);
        assert_eq!(q.enqueue(trigger("E1")), EnqueueOutcome::Enqueued);
        assert_eq!(
            q.enqueue(trigger("E1")),
            EnqueueOutcome::Duplicate,
            "the second trigger with the same event_id must collapse"
        );
        // Exactly one item reached the channel.
        assert!(consumer.try_recv().is_some());
        assert!(
            consumer.try_recv().is_none(),
            "the duplicate must not have been enqueued"
        );
    }

    #[test]
    fn distinct_event_ids_each_enqueue() {
        let (q, _consumer) = DispatchQueue::new(16, 64);
        assert_eq!(q.enqueue(trigger("E1")), EnqueueOutcome::Enqueued);
        assert_eq!(q.enqueue(trigger("E2")), EnqueueOutcome::Enqueued);
        assert_eq!(q.seen_event_ids(), vec!["E1".to_owned(), "E2".to_owned()]);
    }

    #[test]
    fn dedup_is_shared_across_clones() {
        let (q, mut consumer) = DispatchQueue::new(16, 64);
        let q2 = q.clone();
        assert_eq!(q.enqueue(trigger("E1")), EnqueueOutcome::Enqueued);
        assert_eq!(
            q2.enqueue(trigger("E1")),
            EnqueueOutcome::Duplicate,
            "a clone must see the same seen-set"
        );
        assert!(consumer.try_recv().is_some());
        assert!(consumer.try_recv().is_none());
    }

    #[test]
    fn full_queue_returns_full_and_does_not_record() {
        // Capacity 1: first enqueue fills the channel; the second (new
        // event_id) hits backpressure.
        let (q, mut consumer) = DispatchQueue::new(1, 64);
        assert_eq!(q.enqueue(trigger("E1")), EnqueueOutcome::Enqueued);
        assert_eq!(
            q.enqueue(trigger("E2")),
            EnqueueOutcome::Full,
            "a full channel must surface backpressure"
        );
        // E2 was not recorded, so once room frees up it can be enqueued.
        assert!(consumer.try_recv().is_some());
        assert_eq!(
            q.enqueue(trigger("E2")),
            EnqueueOutcome::Enqueued,
            "a Full trigger is retryable once room frees up"
        );
        assert_eq!(q.seen_event_ids(), vec!["E1".to_owned(), "E2".to_owned()]);
    }

    #[test]
    fn seen_set_is_bounded_and_evicts_fifo() {
        let (q, mut consumer) = DispatchQueue::new(64, 2);
        for id in ["E1", "E2", "E3"] {
            assert_eq!(q.enqueue(trigger(id)), EnqueueOutcome::Enqueued);
            // Drain so the channel never fills (we are testing the seen-set
            // bound, not channel backpressure).
            let _ = consumer.try_recv();
        }
        // E1 was evicted (cap 2), so it is no longer deduped.
        assert_eq!(
            q.seen_event_ids(),
            vec!["E2".to_owned(), "E3".to_owned()],
            "the oldest event_id is evicted once over seen_cap"
        );
        assert_eq!(
            q.enqueue(trigger("E1")),
            EnqueueOutcome::Enqueued,
            "an evicted event_id is no longer collapsed"
        );
    }
}
