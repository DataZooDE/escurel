//! Shared harness for the runner suite: one way to start a real
//! `escurel-runner` against a real gateway, and the MCP / run-control
//! helpers most files need.
//!
//! Before this existed every file carried its own `ChildGuard` and a
//! hand-rolled `Command` with the same dozen env vars, so a new runner knob
//! meant touching dozens of copies. New tests use [`RunnerProcess`].

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use escurel_test_support::{EscurelProcess, Role, free_port};
use serde_json::{Value, json};

/// How the runner authenticates to the gateway.
enum Auth {
    /// A static bearer (`ESCUREL_RUNNER_TOKEN`).
    Static(String),
    /// Minted mode: the runner holds the gateway's signing key and mints a
    /// per-run token itself. Required wherever a run token's claims matter.
    Minted,
}

/// Builder for a real runner process on the echo harness.
pub struct RunnerProcess<'a> {
    gw: &'a EscurelProcess,
    tenant: String,
    auth: Auth,
    ledger: Option<PathBuf>,
    env: Vec<(String, String)>,
}

impl<'a> RunnerProcess<'a> {
    /// An echo-harness runner for `tenant`, on a static admin bearer, with a
    /// fresh ledger and the fast poll/backoff the suite relies on.
    pub fn echo(gw: &'a EscurelProcess, tenant: &str) -> Self {
        Self {
            gw,
            tenant: tenant.to_owned(),
            auth: Auth::Static(gw.mint_token(tenant, Role::Admin)),
            ledger: None,
            env: vec![
                ("ESCUREL_RUNNER_HARNESS".into(), "echo".into()),
                ("ESCUREL_RUNNER_POLL_INTERVAL".into(), "250ms".into()),
                ("ESCUREL_RUNNER_RETRY_BACKOFF".into(), "50ms".into()),
            ],
        }
    }

    /// Mint per-run tokens with the gateway's signing key (no static bearer).
    pub fn minted(mut self) -> Self {
        self.auth = Auth::Minted;
        self
    }

    /// Use `token` as the runner's static bearer.
    pub fn static_bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = Auth::Static(token.into());
        self
    }

    /// Set (or override) one env var on the runner.
    pub fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.retain(|(k, _)| k != key);
        self.env.push((key.to_owned(), value.into()));
        self
    }

    /// Use an existing ledger file instead of a fresh one.
    pub fn ledger(mut self, path: impl AsRef<Path>) -> Self {
        self.ledger = Some(path.as_ref().to_owned());
        self
    }

    /// Start the runner. The returned guard kills it on drop.
    pub fn spawn(self) -> RunningRunner {
        let (ledger_dir, ledger_path) = match self.ledger {
            Some(path) => (None, path),
            None => {
                let dir = tempfile::tempdir().expect("ledger tempdir");
                let path = dir.path().join("ledger.duckdb");
                (Some(dir), path)
            }
        };
        let mut env = vec![
            (
                "ESCUREL_RUNNER_GATEWAY_URL".to_owned(),
                self.gw.base_url().to_owned(),
            ),
            ("ESCUREL_RUNNER_TENANT".to_owned(), self.tenant.clone()),
            (
                "ESCUREL_RUNNER_LEDGER_PATH".to_owned(),
                ledger_path.display().to_string(),
            ),
        ];
        match self.auth {
            Auth::Static(token) => env.push(("ESCUREL_RUNNER_TOKEN".into(), token)),
            Auth::Minted => {
                let (signing_key, kid) = self.gw.signing_material();
                env.push(("ESCUREL_RUNNER_AUTH_ISSUER".into(), self.gw.issuer_url()));
                env.push(("ESCUREL_RUNNER_AUTH_KID".into(), kid.to_owned()));
                env.push(("ESCUREL_RUNNER_AUTH_SIGNING_KEY".into(), signing_key));
            }
        }
        env.extend(self.env);
        let mut runner = RunningRunner {
            child: None,
            listen: String::new(),
            tenant: self.tenant,
            env,
            _ledger_dir: ledger_dir,
        };
        runner.start();
        runner
    }
}

/// A live runner. Killed (and reaped) on drop; its default ledger lives as
/// long as the guard, so [`RunningRunner::restart`] finds it again.
pub struct RunningRunner {
    child: Option<Child>,
    listen: String,
    tenant: String,
    env: Vec<(String, String)>,
    _ledger_dir: Option<tempfile::TempDir>,
}

impl RunningRunner {
    fn start(&mut self) {
        self.listen = format!("127.0.0.1:{}", free_port());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
        // An inherited bearer would silently switch a minted runner to static.
        cmd.env_remove("ESCUREL_RUNNER_TOKEN")
            .env("ESCUREL_RUNNER_LISTEN", &self.listen);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        self.child = Some(cmd.spawn().expect("spawn escurel-runner"));
    }

    /// The `host:port` the runner's HTTP surface listens on.
    pub fn listen(&self) -> &str {
        &self.listen
    }

    /// `http://` + [`Self::listen`].
    pub fn base_url(&self) -> String {
        format!("http://{}", self.listen)
    }

    /// Stop the process (SIGKILL) and reap it.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Kill and start again with the same env and the same ledger, on a new
    /// port — the runner's view of a crash and reboot.
    pub fn restart(&mut self) {
        self.kill();
        self.start();
    }

    /// Wait until `/healthz` answers. Panics if the process exits first.
    pub async fn wait_healthy(&mut self) {
        let url = format!("{}/healthz", self.base_url());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.exited() {
                panic!("runner exited before it became healthy: {status:?}");
            }
            if let Ok(resp) = reqwest::get(&url).await
                && resp.status().is_success()
            {
                return;
            }
            assert!(Instant::now() < deadline, "runner never became healthy");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// `Some(status)` once the process has exited on its own.
    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child
            .as_mut()
            .and_then(|c| c.try_wait().expect("try_wait"))
    }

    /// The ledger row for `event_id` (`/debug/run`), if the runner has one.
    pub async fn debug_run(&self, event_id: &str) -> Option<Value> {
        let resp = reqwest::get(format!(
            "{}/debug/run?tenant={}&event_id={event_id}",
            self.base_url(),
            self.tenant
        ))
        .await
        .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }

    /// Wait until the run for `event_id` has one of the `want` statuses.
    pub async fn wait_for_status(&self, event_id: &str, want: &[&str], within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            if let Some(run) = self.debug_run(event_id).await
                && want.iter().any(|w| run["status"] == *w)
            {
                return run;
            }
            assert!(Instant::now() < deadline, "run never reached {want:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for RunningRunner {
    fn drop(&mut self) {
        self.kill();
    }
}

/// `tools/call` `name` as `token`; the structured result. Fails the test on
/// a JSON-RPC error.
pub async fn call(gw: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// A system event's JSON body.
pub fn body_of(e: &Value) -> Value {
    serde_json::from_str(e["body"].as_str().unwrap()).unwrap()
}

/// Wait until the run's `run-started` exists: the gateway authorises a
/// run-control request through it, and the runner writes it best-effort
/// right after claiming the run, so a request sent the instant `/debug/run`
/// flips to pending can precede it.
pub async fn wait_for_run_started(gw: &EscurelProcess, token: &str, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let own = call(gw, token, "list_events", json!({ "run_id": run_id })).await;
        if own["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["title"] == "run-started")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no run-started for {run_id}: {own}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// File an `escurel:run-control` request as `token`; its event id.
pub async fn control(gw: &EscurelProcess, token: &str, body: Value) -> String {
    let r = call(
        gw,
        token,
        "capture_event",
        json!({ "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": body["action"],
                "body": body.to_string() }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// The `escurel:run-control-result` the runner wrote for `request_id`.
pub async fn wait_for_result(gw: &EscurelProcess, token: &str, request_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = call(
            gw,
            token,
            "list_events",
            json!({ "label_skill": "escurel:run-control-result" }),
        )
        .await;
        if let Some(e) = r["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["provenance"]["control"]["request_event_id"] == request_id)
        {
            return e.clone();
        }
        assert!(Instant::now() < deadline, "no result for {request_id}: {r}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
