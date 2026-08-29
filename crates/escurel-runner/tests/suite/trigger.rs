//! DoD test for issue #146 + #147: spawn the real `escurel-runner` binary
//! and drive its `POST /trigger` webhook listener over real HTTP. No
//! mocks — a real subprocess, a real TCP listener, real requests (CLAUDE.md
//! principle 2).
//!
//! #146 cases: (1) no secret configured → a realistic serialized `Event`
//! body returns `202`. #147 upgrades the auth model to an HMAC over the
//! raw request body: (2) a secret configured → a POST with a valid
//! `X-Escurel-Webhook-Signature: sha256=<hex>` returns `202`; a tampered
//! body / wrong signature returns `401`; a missing signature returns `401`.
//! The authoritative `tenant_id` now rides in the payload.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

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

/// A realistic webhook body: a serialized [`escurel_types::Event`] plus the
/// authoritative `tenant_id` the gateway injects (#147), as the gateway's
/// signed POST sends it.
const EVENT_BODY: &str = r#"{
        "event_id": "01ABCDEF0123456789",
        "source": "gcal",
        "mime": "text/plain",
        "label_skill": "note",
        "title": "x",
        "body": "y",
        "status": "inbox",
        "tenant_id": "carl"
    }"#;

/// Compute `sha256=<hex>` HMAC over `body`, matching the gateway scheme.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key of any size");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

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
fn trigger_with_secret_enforces_hmac_signature() {
    const SECRET: &str = "s3cret";
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let _guard = spawn_runner(&listen, Some(SECRET));

    let client = reqwest::blocking::Client::new();
    let url = format!("http://{listen}/trigger");

    // No signature → 401.
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger without signature");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "a secured /trigger must reject a request missing the signature"
    );

    // Tampered body (signature over the original, but body altered) → 401.
    let tampered = EVENT_BODY.replace("\"title\": \"x\"", "\"title\": \"tampered\"");
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header(
            "X-Escurel-Webhook-Signature",
            sign(SECRET, EVENT_BODY.as_bytes()),
        )
        .body(tampered)
        .send()
        .expect("POST /trigger with mismatched signature");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "a secured /trigger must reject a body that does not match the signature"
    );

    // Wrong-secret signature over the correct body → 401.
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header(
            "X-Escurel-Webhook-Signature",
            sign("wrong-secret", EVENT_BODY.as_bytes()),
        )
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger with wrong-secret signature");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "a secured /trigger must reject a signature computed with the wrong secret"
    );

    // Valid signature over the exact body → 202.
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header(
            "X-Escurel-Webhook-Signature",
            sign(SECRET, EVENT_BODY.as_bytes()),
        )
        .body(EVENT_BODY)
        .send()
        .expect("POST /trigger with valid signature");
    assert_eq!(
        resp.status().as_u16(),
        202,
        "a secured /trigger must accept a request whose signature matches the body"
    );
}
