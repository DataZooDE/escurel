//! Fan-out of session frames to the other sockets attached to a session.
//!
//! A live session used to be single-peer: `handle_op` replied `op_ack` to the
//! originating socket and the server kept no registry of sockets per session,
//! so no frame type had a path to a second peer. Two devices on one session
//! stayed silently divergent — each learned of the other's edits only by
//! asking again. `ws.rs` had anticipated this for two milestones
//! (*"M4 broadcasts to other connected peers via the LiveSessionDispatcher"*);
//! this is that dispatcher. See #352.
//!
//! The CRDT layer was never the problem — ops merged and persisted correctly.
//! The gap was purely transport, which is why nothing here touches `LiveDoc`.
//!
//! ## Shape
//!
//! One `tokio::sync::broadcast` channel per session id. Each attached socket
//! subscribes on attach and drops its receiver on disconnect; the channel is
//! removed once the last peer leaves, so an idle gateway holds no state for
//! sessions nobody is watching.
//!
//! Broadcast (not a per-peer queue) because the payload is identical for every
//! recipient and the fan-out is small — the peers on one document. It also
//! gives bounded memory per session for free: a slow peer lags rather than
//! growing an unbounded backlog, and lagging is a state we can report (see
//! [`PeerRecv::Lagged`]) instead of a leak we cannot.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::broadcast;

/// How many frames a peer may fall behind before it is told to resync.
///
/// Sized for a burst of typing while a peer is briefly descheduled, not for an
/// offline client: a peer that cannot keep up is better served by resyncing
/// than by replaying a long tail it will merge anyway.
const CHANNEL_CAPACITY: usize = 256;

/// A frame published by one peer, for delivery to the others.
#[derive(Clone, Debug)]
pub(crate) struct PeerFrame {
    /// The connection that published it, so the originator can skip its own
    /// frame — it already received an `op_ack`, and applying its own edit a
    /// second time is exactly the bug a naive broadcast introduces.
    pub from: PeerId,
    pub frame: Arc<Value>,
}

/// Identifies one attached socket for the lifetime of its connection.
///
/// Process-local and never persisted: it exists only to suppress self-echo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PeerId(u64);

/// What a peer's receive attempt yielded.
pub(crate) enum PeerRecv {
    /// A frame from another peer.
    Frame(Arc<Value>),
    /// This peer fell behind by `skipped` frames and must resync.
    ///
    /// Surfaced rather than swallowed: silently dropping frames would leave a
    /// client confidently showing stale content, which is worse than telling
    /// it to re-read. #352 asks that a client can reconcile what it missed or
    /// that the gap be documented — this is the reconcile signal.
    Lagged { skipped: u64 },
    /// The channel closed (last sender gone); nothing further will arrive.
    Closed,
}

/// Per-session broadcast channels.
#[derive(Debug, Default)]
pub(crate) struct LiveSessionDispatcher {
    channels: DashMap<String, broadcast::Sender<PeerFrame>>,
    next_peer: AtomicU64,
}

impl LiveSessionDispatcher {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Allocate an id for a newly attached socket.
    pub(crate) fn next_peer_id(&self) -> PeerId {
        PeerId(self.next_peer.fetch_add(1, Ordering::Relaxed))
    }

    /// Subscribe to `session_id`, creating the channel if this is the first
    /// peer.
    pub(crate) fn subscribe(&self, session_id: &str) -> PeerSubscription {
        let tx = self
            .channels
            .entry(session_id.to_owned())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .clone();
        PeerSubscription { rx: tx.subscribe() }
    }

    /// Publish `frame` to every peer on `session_id` except `from`.
    ///
    /// A send with no receivers is not an error — a single-peer session
    /// publishes into a channel only its own (skipped) sender holds.
    pub(crate) fn publish(&self, session_id: &str, from: PeerId, frame: Value) {
        if let Some(tx) = self.channels.get(session_id) {
            let _ = tx.send(PeerFrame {
                from,
                frame: Arc::new(frame),
            });
        }
    }

    /// Drop the channel for `session_id` when the last peer has left.
    ///
    /// Checked under the map entry so a peer attaching concurrently cannot
    /// have its brand-new channel removed between subscribe and first send.
    pub(crate) fn release(&self, session_id: &str) {
        self.channels
            .remove_if(session_id, |_, tx| tx.receiver_count() == 0);
    }

    /// Number of live session channels — for tests and the metrics gauge.
    #[cfg(test)]
    pub(crate) fn open_channels(&self) -> usize {
        self.channels.len()
    }
}

/// One peer's receiving end.
pub(crate) struct PeerSubscription {
    rx: broadcast::Receiver<PeerFrame>,
}

impl PeerSubscription {
    /// Await the next frame published by a peer other than `me`.
    ///
    /// Frames from `me` are skipped here rather than at publish time so the
    /// sender does not need to know who is listening.
    pub(crate) async fn recv(&mut self, me: PeerId) -> PeerRecv {
        loop {
            match self.rx.recv().await {
                Ok(pf) if pf.from == me => {}
                Ok(pf) => return PeerRecv::Frame(pf.frame),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    return PeerRecv::Lagged { skipped: n };
                }
                Err(broadcast::error::RecvError::Closed) => return PeerRecv::Closed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_frame_reaches_another_peer() {
        let d = LiveSessionDispatcher::new();
        let a = d.next_peer_id();
        let b = d.next_peer_id();
        let mut sub_b = d.subscribe("s1");

        d.publish("s1", a, json!({ "type": "peer_op", "n": 1 }));

        match sub_b.recv(b).await {
            PeerRecv::Frame(f) => assert_eq!(f["n"], 1),
            _ => panic!("b must receive a's frame"),
        }
    }

    #[tokio::test]
    async fn a_peer_never_receives_its_own_frame() {
        let d = LiveSessionDispatcher::new();
        let a = d.next_peer_id();
        let mut sub_a = d.subscribe("s1");

        d.publish("s1", a, json!({ "type": "peer_op", "n": 1 }));
        d.publish("s1", d.next_peer_id(), json!({ "type": "peer_op", "n": 2 }));

        // The first frame is a's own and must be skipped; the next one
        // delivered is the other peer's.
        match sub_a.recv(a).await {
            PeerRecv::Frame(f) => assert_eq!(f["n"], 2, "a's own frame must be skipped"),
            _ => panic!("expected the other peer's frame"),
        }
    }

    #[tokio::test]
    async fn sessions_are_isolated_from_each_other() {
        let d = LiveSessionDispatcher::new();
        let a = d.next_peer_id();
        let b = d.next_peer_id();
        let mut sub_other = d.subscribe("s2");

        d.publish("s1", a, json!({ "type": "peer_op" }));

        // Nothing on s1 may appear on s2. Publishing to s2 afterwards proves
        // the receiver is live, so "nothing arrived" is not a dead channel.
        d.publish("s2", a, json!({ "type": "peer_op", "marker": true }));
        match sub_other.recv(b).await {
            PeerRecv::Frame(f) => assert_eq!(f["marker"], true, "s1's frame leaked into s2"),
            _ => panic!("s2 subscriber must receive s2's frame"),
        }
    }

    #[tokio::test]
    async fn a_channel_is_dropped_when_its_last_peer_leaves() {
        let d = LiveSessionDispatcher::new();
        let sub = d.subscribe("s1");
        assert_eq!(d.open_channels(), 1);

        d.release("s1");
        assert_eq!(d.open_channels(), 1, "a live subscriber keeps the channel");

        drop(sub);
        d.release("s1");
        assert_eq!(d.open_channels(), 0, "the last peer leaving frees it");
    }

    #[tokio::test]
    async fn a_slow_peer_is_told_to_resync_rather_than_silently_losing_frames() {
        let d = LiveSessionDispatcher::new();
        let a = d.next_peer_id();
        let b = d.next_peer_id();
        let mut sub_b = d.subscribe("s1");

        for i in 0..(CHANNEL_CAPACITY + 10) {
            d.publish("s1", a, json!({ "type": "peer_op", "n": i }));
        }

        match sub_b.recv(b).await {
            PeerRecv::Lagged { skipped } => assert!(skipped > 0, "must report how far behind"),
            other => panic!(
                "an overrun peer must be told to resync, not silently served \
                 stale frames (got {})",
                match other {
                    PeerRecv::Frame(_) => "a frame",
                    PeerRecv::Closed => "closed",
                    PeerRecv::Lagged { .. } => unreachable!(),
                }
            ),
        }
    }
}
