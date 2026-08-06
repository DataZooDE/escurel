//! Convergence: the property the collaborative-editing feature rests on.
//!
//! `livedoc_roundtrip.rs` drives a single peer through the actor and proves
//! the actor serialises calls. It does not prove that two *independent*
//! editors — an agent and a browser, each with its own `LoroDoc` — reach the
//! same body. `LoroDoc::new()` appears exactly once in that file, so before
//! this one nothing in the crate tested convergence at all.
//!
//! These tests exercise Loro directly rather than through `LiveDoc`, because
//! convergence is a property of the merge, and `LiveDoc` deliberately owns
//! exactly one doc per page (`SessionManager` enforces one open session per
//! page for that reason). What the server must rely on is that op blobs from
//! independent peers, imported in any order, converge — which is what is
//! asserted here.
//!
//! See `docs/notes/concurrency-fix-plan.md` F3.1 and F3.2.

use anyhow::Result;
use loro::{ExportMode, LoroDoc};

/// One independent editor with its own peer id and oplog.
struct Peer {
    doc: LoroDoc,
    seen: loro::VersionVector,
}

impl Peer {
    fn new() -> Self {
        let doc = LoroDoc::new();
        let seen = doc.oplog_vv();
        Self { doc, seen }
    }

    fn insert(&mut self, pos: usize, text: &str) {
        let body = self.doc.get_text("body");
        let len = body.len_unicode();
        body.insert(pos.min(len), text).unwrap();
        self.doc.commit();
    }

    /// Everything this peer has produced since `seen` was last advanced.
    fn export_since_last(&mut self) -> Vec<u8> {
        let update = self.doc.export(ExportMode::updates(&self.seen)).unwrap();
        self.seen = self.doc.oplog_vv();
        update
    }

    /// The full oplog, for out-of-order delivery: importing the same ops
    /// twice must be a no-op, which is what makes replay safe.
    fn export_all(&self) -> Vec<u8> {
        self.doc.export(ExportMode::all_updates()).unwrap()
    }

    fn import(&mut self, blob: &[u8]) {
        if !blob.is_empty() {
            self.doc.import(blob).unwrap();
        }
    }

    fn body(&self) -> String {
        self.doc.get_text("body").to_string()
    }
}

/// Two peers editing concurrently converge once each has seen the other's
/// ops — regardless of which order they exchange them in.
#[test]
fn two_independent_peers_converge_via_import() -> Result<()> {
    let (mut a, mut b) = (Peer::new(), Peer::new());

    // A common ancestor, so this is a genuine concurrent edit rather than
    // two unrelated documents.
    a.insert(0, "shared base. ");
    let base = a.export_since_last();
    b.import(&base);
    assert_eq!(a.body(), b.body(), "peers must start from the same state");

    // Concurrent, unsynchronised edits at different offsets.
    a.insert(0, "[A] ");
    b.insert(b.body().len(), " [B]");

    let from_a = a.export_since_last();
    let from_b = b.export_since_last();

    // Exchanged in opposite orders relative to each peer's own edit.
    b.import(&from_a);
    a.import(&from_b);

    assert_eq!(
        a.body(),
        b.body(),
        "independent peers must converge after exchanging ops"
    );
    assert!(a.body().contains("[A]"), "A's edit survived: {}", a.body());
    assert!(a.body().contains("[B]"), "B's edit survived: {}", a.body());
    Ok(())
}

/// Both peers inserting at the *same* offset is the case where a naive
/// last-writer-wins merge loses an edit. Neither may be dropped.
#[test]
fn concurrent_inserts_at_the_same_offset_keep_both_edits() -> Result<()> {
    let (mut a, mut b) = (Peer::new(), Peer::new());
    a.insert(0, "0123456789");
    let base = a.export_since_last();
    b.import(&base);

    a.insert(5, "<A>");
    b.insert(5, "<B>");

    let (from_a, from_b) = (a.export_since_last(), b.export_since_last());
    b.import(&from_a);
    a.import(&from_b);

    assert_eq!(a.body(), b.body(), "same-offset inserts must converge");
    assert!(
        a.body().contains("<A>") && a.body().contains("<B>"),
        "neither edit may be lost: {}",
        a.body()
    );
    Ok(())
}

/// Importing the same ops twice must not duplicate them. Replay from the op
/// log depends on this: `LiveDoc::open` re-imports whatever the snapshot did
/// not already cover, and an overlap must be harmless.
#[test]
fn reimporting_the_same_ops_is_idempotent() -> Result<()> {
    let (mut a, mut b) = (Peer::new(), Peer::new());
    a.insert(0, "hello world");
    let all = a.export_all();

    b.import(&all);
    let once = b.body();
    b.import(&all);
    b.import(&all);

    assert_eq!(once, b.body(), "re-import must not duplicate content");
    assert_eq!(a.body(), b.body());
    Ok(())
}

/// Property: N peers, random local edits, exchanged in a random order, all
/// converge to one body.
///
/// Deterministic by construction — the schedule comes from a seeded LCG, not
/// the thread scheduler — so a failure is reproducible from the seed printed
/// in the assertion rather than being a flake to chase.
#[test]
fn prop_random_op_interleavings_converge() -> Result<()> {
    const PEERS: usize = 4;
    const ROUNDS: usize = 12;

    for seed in [1_u64, 7, 42, 1337, 90210] {
        let mut rng = seed;
        let mut next = || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as usize
        };

        let mut peers: Vec<Peer> = (0..PEERS).map(|_| Peer::new()).collect();

        // Common ancestor.
        peers[0].insert(0, "root. ");
        let base = peers[0].export_since_last();
        for p in peers.iter_mut().skip(1) {
            p.import(&base);
        }

        // Random local edits, then a random exchange each round. Ops are
        // deliberately delivered late and out of order.
        let mut pending: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut inserted_chars = "root. ".chars().count();
        for round in 0..ROUNDS {
            let author = next() % PEERS;
            let pos = next() % (peers[author].body().chars().count() + 1);
            let marker = format!("<{author}:{round}>");
            inserted_chars += marker.chars().count();
            peers[author].insert(pos, &marker);
            let blob = peers[author].export_since_last();
            pending.push((author, blob));

            // Deliver a random subset of what is outstanding.
            if !pending.is_empty() && next() % 2 == 0 {
                let idx = next() % pending.len();
                let (from, blob) = pending[idx].clone();
                let to = next() % PEERS;
                if to != from {
                    peers[to].import(&blob);
                }
            }
        }

        // Settle: everyone sees everything.
        let all: Vec<Vec<u8>> = peers.iter().map(|p| p.export_all()).collect();
        for p in peers.iter_mut() {
            for blob in &all {
                p.import(blob);
            }
        }

        let first = peers[0].body();
        for (i, p) in peers.iter().enumerate() {
            assert_eq!(
                p.body(),
                first,
                "seed {seed}: peer {i} diverged\n  peer0: {first}\n  peer{i}: {}",
                p.body()
            );
        }
        // No content was lost. Marker *substrings* are not a valid check:
        // concurrent inserts may legitimately interleave inside one another,
        // splitting `<0:3>` into `<0:<1:4>3>`. That is correct CRDT
        // behaviour, not loss. What must hold is that inserts never delete,
        // so every character ever inserted is still present.
        assert_eq!(
            first.chars().count(),
            inserted_chars,
            "seed {seed}: converged body lost or duplicated characters\n  body: {first}"
        );
    }
    Ok(())
}
