//! DoD test for issue #146: spawn the real `escurel-runner` binary and
//! drive its `POST /trigger` webhook listener over real HTTP. No mocks —
//! a real subprocess, a real TCP listener, real requests (CLAUDE.md
//! principle 2).
//!
//! Two cases: (1) no secret configured → a realistic serialized `Event`
//! body returns `202`; (2) a secret configured → a POST without the
//! header returns `401`, with the correct header returns `202`.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Kills the spawned runner on drop so a test failure never orphans the
/// process.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Bind to port 0 to let the OS pick a free port, read it, then drop the
/// listener so the runner can claim it.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("read local_addr").port()
}

/// A realistic webhook body: a serialized [`escurel_types::Event`] as the
/// gateway's `client.post(url).json(&event)` would send it.
const EVENT_BODY: &str = r#"{
        "event_id": "01ABCDEF0123456789",
        "source": "gcal",
        "mime": "text/plain",
        "label_skill": "note",
        "title": "x",
        "body": "y",
        "status": "inbox"
    }"#;

/// Spawn the runner, optionally with a webhook secret, and wait until it
/// answers `/healthz`.
fn spawn_runner(listen: &str, secret: Option<&str>) -> ChildGuard {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", listen);
    if let Some(secret) = secret {
        cmd.env("ESCUREL_WEBHOOK_SECRET", secret);
    }
    let child = cmd.spawn().expect("spawn escurel-runner binary");
    let guard = ChildGuard(child);

    let health_url = format!("http://{listen}/healthz");
    let client = reqwest::blocking::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client.get(&health_url).send() {
            Ok(resp) if resp.status().is_success() => break,
            _ => {
                if Instant::now() >= deadline {
                    panic!("runner never became healthy at {health_url} within 10s");
                }
                let _ = ErrorKind::ConnectionRefused;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    guard
}

#[test]
fn trigger_without_secret_accepts_event() {
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let _guard = spawn_runner(&listen, None);

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("http://{listen}/trigger"))
        .header("content-type", "application/json")
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger");
    assert_eq!(
        resp.status().as_u16(),
        202,
        "an unsecured /trigger must accept the event with 202"
    );
}

#[test]
fn trigger_with_secret_enforces_header() {
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let _guard = spawn_runner(&listen, Some("s3cret"));

    let client = reqwest::blocking::Client::new();
    let url = format!("http://{listen}/trigger");

    // No header → 401.
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger without secret");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "a secured /trigger must reject a request missing the secret header"
    );

    // Correct header → 202.
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header("X-Escurel-Webhook-Secret", "s3cret")
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger with secret");
    assert_eq!(
        resp.status().as_u16(),
        202,
        "a secured /trigger must accept a request with the matching secret header"
    );
}
