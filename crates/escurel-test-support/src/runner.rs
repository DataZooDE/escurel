//! A real `escurel-runner` process for integration tests, next to
//! [`crate::EscurelProcess`]: one builder for the env, the ledger, the
//! harness and minted vs static auth, and a guard that kills it on drop.
//!
//! Moved here from the runner's own suite so any crate whose test needs a
//! runner (the chat bridge's round-trip, for one) starts it the same way.
//!
//! The binary is found the way a cargo test sees it: `ESCUREL_RUNNER_BIN`
//! when set, else `escurel-runner` beside the test executable's target
//! directory (`target/<profile>/`). `cargo test --workspace` builds it; a
//! single-crate run outside the runner needs `cargo build -p escurel-runner`
//! first (it also needs the `escurel-echo-harness` sibling for the echo
//! harness).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{EscurelProcess, Role, free_port};

/// The `escurel-runner` binary the tests start.
///
/// # Panics
/// When neither `ESCUREL_RUNNER_BIN` nor a built sibling exists.
#[must_use]
pub fn runner_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("ESCUREL_RUNNER_BIN") {
        return PathBuf::from(path);
    }
    // A test executable lives in `target/<profile>/deps/`; the binaries one up.
    let exe = std::env::current_exe().expect("current_exe");
    let candidate = exe
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join(format!("escurel-runner{}", std::env::consts::EXE_SUFFIX)));
    match candidate {
        Some(path) if path.exists() => path,
        _ => panic!(
            "no escurel-runner binary next to {}: run `cargo build -p escurel-runner` \
             (or set ESCUREL_RUNNER_BIN)",
            exe.display()
        ),
    }
}

/// How the runner authenticates to the gateway.
enum Auth {
    /// A static bearer (`ESCUREL_RUNNER_TOKEN`).
    Static(String),
    /// Minted mode: the runner holds the gateway's signing key and mints a
    /// per-run token itself. Required wherever a run token's claims matter.
    Minted,
    /// A bearer read from a file (`ESCUREL_RUNNER_TOKEN_FILE`).
    File(PathBuf),
}

/// Builder for a real runner process on the echo harness.
pub struct RunnerProcess<'a> {
    gw: &'a EscurelProcess,
    tenant: String,
    auth: Auth,
    ledger: Option<PathBuf>,
    bin: Option<PathBuf>,
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
            bin: None,
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

    /// Read the runner's bearer from `path` (`ESCUREL_RUNNER_TOKEN_FILE`),
    /// which the test may rewrite under the live runner.
    pub fn token_file(mut self, path: impl AsRef<Path>) -> Self {
        self.auth = Auth::File(path.as_ref().to_owned());
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

    /// Start this binary instead of [`runner_bin`]'s (the runner's own suite
    /// passes `CARGO_BIN_EXE_escurel-runner`).
    pub fn binary(mut self, path: impl AsRef<Path>) -> Self {
        self.bin = Some(path.as_ref().to_owned());
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
            Auth::File(path) => env.push((
                "ESCUREL_RUNNER_TOKEN_FILE".into(),
                path.display().to_string(),
            )),
            Auth::Minted => {
                let (signing_key, kid) = self.gw.signing_material();
                env.push(("ESCUREL_RUNNER_AUTH_ISSUER".into(), self.gw.issuer_url()));
                env.push(("ESCUREL_RUNNER_AUTH_KID".into(), kid.to_owned()));
                env.push(("ESCUREL_RUNNER_AUTH_SIGNING_KEY".into(), signing_key));
            }
        }
        env.extend(self.env);
        let mut runner = RunningRunner {
            bin: self.bin.unwrap_or_else(runner_bin),
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
    bin: PathBuf,
    child: Option<Child>,
    listen: String,
    tenant: String,
    env: Vec<(String, String)>,
    _ledger_dir: Option<tempfile::TempDir>,
}

impl RunningRunner {
    fn start(&mut self) {
        self.listen = format!("127.0.0.1:{}", free_port());
        let mut cmd = Command::new(&self.bin);
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
