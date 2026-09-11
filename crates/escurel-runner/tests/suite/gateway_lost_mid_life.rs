//! A gateway that goes away is not a run that failed, either (#440's sibling).
//!
//! #440 taught the runner to wait for a gateway that has not started yet. This
//! is the same fact arriving later, and it was NOT covered: measured in the lab
//! on 2026-09-11, a rollout replaced `heron-escurel` while the runner was
//! already up and past its boot wait. The new gateway spent 17 minutes adopting
//! its DuckLake index; the runner, three attempts and a short backoff, dead-
//! lettered **eight in-flight events in seconds** — every one of them healthy
//! work whose only problem was a dependency mid-restart.
//!
//! The distinction the fix rests on: `max_attempts` bounds how many times a RUN
//! is worth trying, and a refused connection says nothing about the run.
//! `ReconcileError::Unavailable` therefore holds the run and waits, spending
//! `unavailable_grace` (30m, the gateway's own startup budget) rather than the
//! attempt budget.
//!
//! Real runner binary, real HTTP, real sockets. The "gateway" here answers
//! `list_skills` and then stops listening — a network fault, not a mock of the
//! gateway's behaviour.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// Enough of a gateway for `await_gateway` to accept it: one `list_skills`
/// answer, MCP-shaped. Everything past the boot wait is what this test takes
/// away.
fn serve_until_stopped(listener: TcpListener, stop: &Arc<AtomicBool>, served: &Arc<AtomicUsize>) {
    listener
        .set_nonblocking(true)
        .expect("non-blocking so the stop flag is reachable");
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                served.fetch_add(1, Ordering::SeqCst);
                answer(stream);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
    // Dropping the listener is the point: from here the port refuses
    // connections, which is what a gateway being replaced looks like.
    drop(listener);
}

fn answer(mut stream: TcpStream) {
    let mut buf = [0_u8; 4096];
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.read(&mut buf);
    let body = r#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"skills":[]}}}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

const EVENT_BODY: &str = r#"{
        "event_id": "01EVENTDURINGAROLLOUT",
        "source": "gcal",
        "mime": "text/plain",
        "label_skill": "note",
        "title": "x",
        "body": "y",
        "status": "inbox",
        "tenant_id": "carl"
    }"#;

#[test]
fn a_gateway_that_disappears_mid_life_does_not_dead_letter_the_work() {
    let ledger = tempfile::tempdir().expect("tempdir");
    let listen = format!("127.0.0.1:{}", free_port());
    let gateway_port = free_port();
    let gateway = TcpListener::bind(("127.0.0.1", gateway_port)).expect("bind the stub gateway");

    let stop = Arc::new(AtomicBool::new(false));
    let served = Arc::new(AtomicUsize::new(0));
    let server = {
        let (stop, served) = (Arc::clone(&stop), Arc::clone(&served));
        std::thread::spawn(move || serve_until_stopped(gateway, &stop, &served))
    };

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env(
            "ESCUREL_RUNNER_GATEWAY_URL",
            format!("http://127.0.0.1:{gateway_port}"),
        )
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
        // Short, so the OLD behaviour exhausts its budget well inside this
        // test: two attempts, 200ms apart, against a connection that is
        // refused immediately.
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
        assert!(Instant::now() < deadline, "the runner must come up");
        std::thread::sleep(Duration::from_millis(100));
    }

    // The premise, and what makes this test different from the boot-time one:
    // the runner got PAST its gateway wait, which it can only do by talking to
    // something. Without this the test would pass against a runner that never
    // started dispatching at all.
    assert!(
        served.load(Ordering::SeqCst) > 0,
        "the runner must have reached the gateway at boot — otherwise this is \
         #440's case again, not its sibling"
    );

    // Now take the gateway away, exactly as a rollout does.
    stop.store(true, Ordering::SeqCst);
    server.join().expect("stub gateway thread");
    let refused = TcpStream::connect_timeout(
        &format!("127.0.0.1:{gateway_port}").parse().unwrap(),
        Duration::from_millis(250),
    );
    assert!(
        refused.is_err(),
        "premise: the port must actually be refusing connections now, or the \
         run below is not facing an absent gateway"
    );

    let accepted = client
        .post(format!("http://{listen}/trigger"))
        .header("content-type", "application/json")
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger");
    assert_eq!(accepted.status().as_u16(), 202, "the trigger is accepted");

    // Comfortably past 2 attempts at 200ms: the old behaviour had dead-lettered
    // by now.
    std::thread::sleep(Duration::from_secs(6));

    let ledger: serde_json::Value = client
        .get(format!("http://{listen}/debug/ledger"))
        .send()
        .expect("GET /debug/ledger")
        .json()
        .expect("ledger json");

    assert_eq!(
        ledger["dead_letter"], 0,
        "a gateway that went away mid-rollout must not dead-letter the work it \
         was carrying — that is the eight events, and they were all fine: {ledger}"
    );
    assert_eq!(
        ledger["failed"], 0,
        "nor record the run failed: it has not been tried: {ledger}"
    );
    assert_eq!(
        ledger["total"], 1,
        "the trigger stays on the books, held for the gateway's return: {ledger}"
    );
    assert_eq!(ledger["terminal"], 0, "nothing terminal happened: {ledger}");
}
