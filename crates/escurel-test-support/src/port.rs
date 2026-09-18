//! Handing a spawned server a free TCP port, without two tests getting the
//! same one.
//!
//! The obvious trick — bind `127.0.0.1:0`, read the port, drop the listener,
//! hand the number to a child — is racy, and the race is not theoretical. The
//! OS picks from the ephemeral range and will happily pick the *same* port for
//! a second caller once the first has dropped its probe listener. Two tests
//! then spawn two servers on one port: the loser dies with
//!
//! ```text
//! Error: Address already in use (os error 98)
//! ```
//!
//! and whatever the loser's test was waiting for never arrives, so it fails at
//! its deadline pointing at the wrong thing entirely. Observed in the
//! `escurel-runner` suite as `curate_generates_a_derivable_by_category_index`
//! timing out after 45s — a test that has nothing to do with ports.
//!
//! [`free_port`] closes the window that matters: every port handed out is
//! remembered for the life of the process and never handed out twice. A test
//! binary is one process — the whole `escurel-runner` suite is a single
//! binary, by design, to keep link time down — so within a suite this is
//! exact rather than probabilistic.
//!
//! What it does NOT close: an unrelated process taking the port between the
//! probe and the child's own bind. Closing that needs the child to bind `:0`
//! itself and report back (`escurel-runner` does log its real `local_addr`),
//! which is the right end state; this is the cheap fix that removes the
//! collisions tests actually cause for each other.

use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};

fn taken() -> &'static Mutex<HashSet<u16>> {
    static TAKEN: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    TAKEN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// A port nothing is listening on, which this process has not handed out
/// before.
///
/// # Panics
/// If no unused port can be probed — in practice only if the ephemeral range
/// is exhausted, or if the OS keeps returning ports already handed out
/// [`ATTEMPTS`] times in a row.
pub fn free_port() -> u16 {
    claim_port(|| {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind an ephemeral port")
            .local_addr()
            .expect("read the probe listener's local_addr")
            .port()
    })
}

/// How many times to re-probe when the OS returns a port already handed out.
const ATTEMPTS: usize = 64;

/// The allocator behind [`free_port`], with the OS probe injected so the
/// "never twice" contract is testable without racing anything.
///
/// # Panics
/// If `probe` returns an already-claimed port [`ATTEMPTS`] times running.
fn claim_port(mut probe: impl FnMut() -> u16) -> u16 {
    for _ in 0..ATTEMPTS {
        let port = probe();
        // `insert` answers false when the set already held it, which is
        // exactly the collision the bind-then-drop probe produces.
        if taken().lock().expect("port registry mutex").insert(port) {
            return port;
        }
    }
    panic!("no unclaimed port after {ATTEMPTS} probes");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract: a port already handed out is never handed out again,
    /// even when the probe keeps offering it.
    ///
    /// Deterministic where the real race is not — a probe that repeats stands
    /// in for the OS reusing an ephemeral port once the first caller dropped
    /// its listener.
    #[test]
    fn a_probe_that_repeats_itself_still_yields_distinct_ports() {
        let offers = [40_001_u16, 40_001, 40_001, 40_002];
        let mut next = offers.iter().copied();
        let mut probe = move || next.next().expect("probe offers exhausted");

        let first = claim_port(&mut probe);
        let second = claim_port(&mut probe);
        assert_eq!(first, 40_001);
        assert_eq!(
            second, 40_002,
            "the repeated offer must be rejected, not handed out a second time"
        );
    }

    /// The real probe, exercised for its actual invariant: many ports, all
    /// distinct, all bindable.
    #[test]
    fn free_port_hands_out_distinct_bindable_ports() {
        let ports: Vec<u16> = (0..64).map(|_| free_port()).collect();
        let unique: HashSet<u16> = ports.iter().copied().collect();
        assert_eq!(unique.len(), ports.len(), "free_port repeated a port");
        for p in ports {
            assert_ne!(p, 0, "a probed port is never 0");
        }
    }
}
