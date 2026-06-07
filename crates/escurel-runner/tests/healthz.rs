//! DoD test for issue #145: spawn the real `escurel-runner` binary as a
//! process, issue a real `GET /healthz` over HTTP, assert `200` + body
//! `OK`. No mocks — a real subprocess, a real TCP listener, real
//! requests (CLAUDE.md principle 2).

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
/// listener so the runner can claim it. (Inherently racy, but the window
/// is tiny and this is the standard ephemeral-port trick.)
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("read local_addr").port()
}

#[test]
fn runner_binary_serves_healthz() {
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");

    let child = Command::new(env!("CARGO_BIN_EXE_escurel-runner"))
        .env("ESCUREL_RUNNER_LISTEN", &listen)
        .spawn()
        .expect("spawn escurel-runner binary");
    let _guard = ChildGuard(child);

    let url = format!("http://{listen}/healthz");
    let client = reqwest::blocking::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);

    let resp = loop {
        match client.get(&url).send() {
            Ok(resp) => break resp,
            Err(e) => {
                if Instant::now() >= deadline {
                    panic!("runner never answered {url} within 10s: {e}");
                }
                // Connection refused while the server is still booting is
                // expected; retry until the deadline.
                let _ = ErrorKind::ConnectionRefused;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };

    assert_eq!(resp.status().as_u16(), 200, "healthz must return 200");
    let body = resp.text().expect("read healthz body");
    assert_eq!(body, "OK", "healthz body must be OK");
}
