//! A gateway that has not started is not a run that failed (#440).
//!
//! Measured in the lab: `escurel-runner` was deployed in the same rollout
//! that changed `heron-escurel`'s values, so the gateway restarted at the
//! moment the runner first booted. The gateway rebuilds a DuckLake index over
//! Google Drive at boot — its own platform budgets 29 minutes for that, and
//! 16 were observed — while `max_attempts` is 3 with a short backoff. The
//! runner found six real inbox events and dead-lettered every one of them
//! within seconds of starting.
//!
//! Three attempts over a few seconds, against a dependency allowed half an
//! hour to come up. The retry policy is right for a run that FAILED and wrong
//! for a dependency that has not started, and the events are the cost:
//! dead-lettered work that nothing was wrong with.
//!
//! Real runner binary, real HTTP, no gateway. No mocks.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
}

/// A trigger the runner will admit into its ledger without asking anyone:
/// the gate is `begin_run` + the loop controls, and none of them touch the
/// gateway. Reaching the gateway is what DISPATCH does, which is the part
/// under test.
const EVENT_BODY: &str = r#"{
        "event_id": "01EVENTWAITINGONABOOT",
        "source": "gcal",
        "mime": "text/plain",
        "label_skill": "note",
        "title": "x",
        "body": "y",
        "status": "inbox",
        "tenant_id": "carl"
    }"#;

#[test]
fn a_trigger_waits_for_a_gateway_that_is_still_booting() {
    let ledger = tempfile::tempdir().expect("tempdir");
    let listen = format!("127.0.0.1:{}", free_port());
    // A port with nothing on it: connection refused, which is what a booting
    // gateway looks like from here.
    let absent_gateway = format!("http://127.0.0.1:{}", free_port());

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", &absent_gateway)
        .env("ESCUREL_RUNNER_TENANT", "carl")
        .env(
            "ESCUREL_RUNNER_TOKEN",
            "not-a-real-bearer-but-a-present-one",
        )
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger.path().join("ledger.sqlite").to_str().unwrap(),
        )
        // Short, so the old behaviour would have burned the budget well
        // inside this test rather than after it.
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "200ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "500ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let client = reqwest::blocking::Client::new();
    let health = format!("http://{listen}/healthz");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if client
            .get(&health)
            .send()
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the runner must answer /healthz even with no gateway — a \
             dependency-free liveness probe is the whole point of one"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let accepted = client
        .post(format!("http://{listen}/trigger"))
        .header("content-type", "application/json")
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger");
    assert_eq!(accepted.status().as_u16(), 202, "the trigger is accepted");

    // Long enough that the old behaviour had finished: 2 attempts, 200ms
    // backoff, and a connection refused returns immediately.
    std::thread::sleep(Duration::from_secs(5));

    let ledger: serde_json::Value = client
        .get(format!("http://{listen}/debug/ledger"))
        .send()
        .expect("GET /debug/ledger")
        .json()
        .expect("ledger json");

    assert_eq!(
        ledger["failed"], 0,
        "a gateway that never answered must not produce a FAILED run — the \
         run has not been attempted, it is waiting: {ledger}"
    );
    assert_eq!(
        ledger["dead_letter"], 0,
        "and must not dead-letter it: dead-lettering is for work that cannot \
         succeed, and this work has not been tried: {ledger}"
    );
    assert_eq!(
        ledger["total"], 1,
        "the trigger is still ON the books — held, not dropped, so the event \
         is there when the gateway is: {ledger}"
    );
    assert_eq!(ledger["terminal"], 0, "nothing terminal happened: {ledger}");
}
