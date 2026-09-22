//! The runner never dispatches a `system` event, nor anything under the
//! reserved `escurel:` label namespace (knowledge-workbench backend, P1
//! PR3 — BRD FR-R-3 / ACC-5).
//!
//! `kind: system` events are bookkeeping ABOUT runs — `run-started`, a
//! review transition, runner health. The gateway hides them from the inbox
//! the poller reads, so the poll path is covered by the gateway filter; the
//! **webhook** path is not — `POST /trigger` hands the runner the raw event
//! — and a runner that dispatched its own `run-finished` would hand itself
//! a job per run, forever. This drives the real binary's `/trigger` over
//! real HTTP (no gateway needed: the gate runs before anything needs one)
//! and reads the real ledger back through `/debug/ledger` + `/debug/seen`.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::free_port;
use serde_json::{Value, json};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_runner(listen: &str, ledger_dir: &std::path::Path) -> ChildGuard {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    // Its OWN ledger — the default path is one file shared by every test
    // (see `echo_end_to_end.rs`), and this test counts rows.
    cmd.env("ESCUREL_RUNNER_LISTEN", listen).env(
        "ESCUREL_RUNNER_LEDGER_PATH",
        ledger_dir.join("ledger.sqlite").to_str().unwrap(),
    );
    let guard = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));
    let health = format!("http://{listen}/healthz");
    let client = reqwest::blocking::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(resp) = client.get(&health).send()
            && resp.status().is_success()
        {
            break;
        }
        assert!(Instant::now() < deadline, "runner never became healthy");
        std::thread::sleep(Duration::from_millis(100));
    }
    guard
}

fn event(event_id: &str, kind: &str, label_skill: &str) -> Value {
    json!({
        "event_id": event_id,
        "kind": kind,
        "source": "test",
        "mime": "text/plain",
        "label_skill": label_skill,
        "title": "t",
        "body": "b",
        "status": "inbox",
        "tenant_id": "carl",
    })
}

#[test]
fn system_and_reserved_label_events_create_no_ledger_row() {
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let _guard = spawn_runner(&listen, ledger_dir.path());
    let client = reqwest::blocking::Client::new();
    let post = |body: Value| {
        let resp = client
            .post(format!("http://{listen}/trigger"))
            .json(&body)
            .send()
            .expect("POST /trigger");
        // The gate ACKs everything it drops — the gateway's POST must never
        // block or retry on a policy decision.
        assert_eq!(resp.status().as_u16(), 202, "{body}");
    };

    // Bookkeeping under an ordinary label: dropped for its KIND.
    post(event("SYS-KIND", "system", "email"));
    // A user-kind event under the reserved namespace: dropped for its LABEL
    // (the pre-existing `escurel:run-status` rule, generalised).
    post(event("RESERVED-LABEL", "user", "escurel:run"));
    // Positive control — an ordinary event must still create its run.
    post(event("CONTROL", "user", "email"));

    // The gate runs synchronously before the 202, so the ledger is final.
    let ledger: Value = client
        .get(format!("http://{listen}/debug/ledger"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        ledger["total"], 1,
        "only the control creates a run: {ledger}"
    );
    let seen: Value = client
        .get(format!("http://{listen}/debug/seen"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let ids: Vec<&str> = seen["event_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(ids, vec!["CONTROL"], "{seen}");
}
