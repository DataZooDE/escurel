//! The [`Harness`] adapter trait + its structured outcome/error types.

use async_trait::async_trait;
use escurel_runner_core::TaskContext;

/// Terminal status of a harness run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessStatus {
    /// The harness completed and its escurel writes were confirmed.
    Ok,
    /// The harness ran but reported a non-fatal failure (its work did not
    /// complete). The reconciler (#155) decides retry-vs-dead.
    Failed,
}

/// The structured result of one harness run.
///
/// Captured by the adapter from the harness subprocess's exit + stdout and
/// handed back to the runner's reconciler. Enough for the minimal reconcile
/// in #151 and the richer retry/cascade logic in #155+ to act on without a
/// second round-trip.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HarnessOutcome {
    /// Whether the run succeeded.
    pub ok: bool,
    /// Terminal status (mirrors [`HarnessOutcome::ok`] but names the
    /// failure mode explicitly for the reconciler).
    pub status: HarnessStatus,
    /// A short human-readable summary of what the harness did.
    pub summary: String,
    /// How many `/mcp` tool calls the harness made.
    pub tool_calls: u32,
    /// The instance page the harness wrote to, if it materialised/updated
    /// one. `None` when the run made no instance write. The reconciler reads
    /// this to confirm the produced state.
    pub produced_instance: Option<String>,
    /// The out-of-band artifact a PRODUCING harness materialised, as a
    /// serialized [`escurel_types::ResultRef`] (`{"kind":…,"scenario_id":…}`) —
    /// e.g. the scenario what-if harness's parquet result (async-ops Phase 4).
    /// `None` for every harness that produces no such artifact. The runner
    /// stamps it onto the operation's terminal `succeeded` status event so
    /// `get_operation` can surface it. Carried as an opaque JSON value here so
    /// this wire crate needs no `escurel-types` dependency; the runner validates
    /// it into the closed enum before stamping. `#[serde(default)]` keeps the
    /// subprocess wire contract back-compatible with harnesses that omit it.
    #[serde(default)]
    pub result_ref: Option<serde_json::Value>,
}

/// Errors raised by a harness adapter while managing its subprocess.
///
/// These are *adapter-level* failures (could not spawn, timed out, the
/// child crashed, its stdout was unparseable) — distinct from a harness that
/// ran cleanly but reported [`HarnessStatus::Failed`] in its
/// [`HarnessOutcome`].
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The harness subprocess could not be spawned.
    #[error("could not spawn harness {harness:?} at {path:?}: {source}")]
    Spawn {
        /// The adapter name.
        harness: &'static str,
        /// The binary path the adapter tried to launch.
        path: String,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The harness ran past its deadline and was killed.
    #[error("harness {harness:?} timed out after {timeout_ms}ms")]
    Timeout {
        /// The adapter name.
        harness: &'static str,
        /// The configured timeout.
        timeout_ms: u64,
    },
    /// The harness exited non-zero (and emitted no parseable outcome).
    #[error("harness {harness:?} exited with status {code:?}: {stderr}")]
    NonZeroExit {
        /// The adapter name.
        harness: &'static str,
        /// The process exit code (`None` if killed by a signal).
        code: Option<i32>,
        /// Captured stderr (truncated) for diagnosis.
        stderr: String,
    },
    /// An in-process (non-subprocess) harness could not reach, or was
    /// refused by, an upstream it depends on — the model API or the gateway
    /// `/mcp`. Distinct from [`HarnessError::NonZeroExit`], which is a
    /// subprocess concept and says nothing about which upstream failed.
    #[error("harness {harness:?} upstream error: {message}")]
    Upstream {
        /// The adapter name.
        harness: &'static str,
        /// What failed, carrying the upstream's own message where there is
        /// one — a bare status code has cost real debugging time here.
        message: String,
    },
    /// The harness stdout was not the expected JSON outcome.
    #[error("could not parse harness {harness:?} outcome: {source}")]
    BadOutcome {
        /// The adapter name.
        harness: &'static str,
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
    },
    /// The adapter cannot run THIS task safely, though the harness itself is
    /// available. Raised before anything is spawned.
    ///
    /// It exists for [`crate::AgyHarness`]: `agy` offers no way to narrow the
    /// tool surface, so a run packaged under
    /// [`REVIEW_TOOLS`](escurel_runner_core::REVIEW_TOOLS) would reach the
    /// committing verbs anyway. Refusing is the only way the autonomy gate
    /// stays a gate. Distinct from every other variant here, which report
    /// that the harness FAILED — this one reports it was never asked.
    #[error("harness {harness:?} cannot run this task: {reason}")]
    Unsupported {
        /// The adapter name.
        harness: &'static str,
        /// Why, in terms the operator can act on — including which harness
        /// can run the task instead.
        reason: String,
    },
    /// An I/O error writing the task to / reading the result from the child.
    #[error("harness {harness:?} I/O error: {source}")]
    Io {
        /// The adapter name.
        harness: &'static str,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// A harness adapter: spawns a real agent harness as a subprocess, points
/// it at the gateway `/mcp` with the packaged scoped token + skill
/// instructions, and captures a structured [`HarnessOutcome`].
///
/// Implementors **must not** write to escurel directly — every escurel
/// effect flows through the harness's own `/mcp` tool calls. The adapter is
/// only process management + outcome capture.
#[async_trait]
pub trait Harness: Send + Sync {
    /// The adapter's stable name (`"echo"`, `"claude"`, `"codex"`, `"agy"`,
    /// `"gemini"`). Used for harness selection (`ESCUREL_RUNNER_HARNESS`) and
    /// logging.
    fn name(&self) -> &str;

    /// Run the packaged task: spawn the harness subprocess, wait (with a
    /// timeout + kill-on-drop), and return its captured outcome.
    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError>;
}

/// One subprocess harness run: spawn, feed stdin, wait against a deadline,
/// check the exit status.
///
/// Five adapters carried their own copy of this sequence — `agy`, `claude`,
/// `codex`, `muse` and `echo` — differing only in the binary, the argv, the
/// env vars and the stdin payload. The copies were identical down to the
/// comment wording, which is the tell: `kill_on_drop`, the EOF-on-stdin hang
/// fix and the 2000-character stderr truncation were five independent
/// implementations of the same safety properties, and a fix to one fixed one.
///
/// `gemini` is deliberately not a caller: it speaks HTTP rather than spawning
/// anything, so it shares none of this.
pub(crate) struct Spawn<'a> {
    /// Adapter name, for the error variants.
    pub harness: &'static str,
    pub bin: &'a str,
    pub args: &'a [String],
    /// Env vars to set on the child. Used to hand a bearer to the child's
    /// environment rather than its argv or an on-disk config, and to point a
    /// harness at its per-run home directory.
    pub envs: Vec<(&'a str, std::ffi::OsString)>,
    /// Written to the child's stdin, which is then CLOSED. Every harness here
    /// reads its prompt until EOF, so a held-open stdin hangs the run until
    /// the timeout — that is why this closes rather than leaving the handle.
    pub stdin: Option<&'a [u8]>,
    pub timeout: std::time::Duration,
}

/// Run `spawn` to completion and return its captured output.
///
/// # Errors
/// [`HarnessError::Spawn`] if the binary will not start, [`HarnessError::Io`]
/// on a stdin or wait failure, [`HarnessError::Timeout`] if it outruns the
/// deadline, and [`HarnessError::NonZeroExit`] (with stderr truncated to 2000
/// characters) if it exits non-zero.
pub(crate) async fn run_capture(spawn: Spawn<'_>) -> Result<std::process::Output, HarnessError> {
    use std::process::Stdio;

    let harness = spawn.harness;
    let mut cmd = tokio::process::Command::new(spawn.bin);
    cmd.args(spawn.args);
    for (k, v) in &spawn.envs {
        cmd.env(k, v);
    }
    // `kill_on_drop` ties the child's lifetime to this future: a dropped
    // adapter (panic, cancellation, timeout) reaps the subprocess.
    let mut child = cmd
        // `null` rather than `piped` when there is nothing to write: a piped
        // stdin nobody closes is the hang this helper exists to prevent, and
        // one adapter (`muse`) genuinely takes its prompt via a file path.
        .stdin(match spawn.stdin {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| HarnessError::Spawn {
            harness,
            path: spawn.bin.to_owned(),
            source,
        })?;

    // Write the payload, then drop the handle to send EOF.
    if let Some(payload) = spawn.stdin {
        let mut stdin = child.stdin.take().ok_or_else(|| HarnessError::Io {
            harness,
            source: std::io::Error::other("stdin was not piped"),
        })?;
        tokio::io::AsyncWriteExt::write_all(&mut stdin, payload)
            .await
            .map_err(|source| HarnessError::Io { harness, source })?;
        tokio::io::AsyncWriteExt::shutdown(&mut stdin)
            .await
            .map_err(|source| HarnessError::Io { harness, source })?;
    }

    let output = match tokio::time::timeout(spawn.timeout, child.wait_with_output()).await {
        Ok(result) => result.map_err(|source| HarnessError::Io { harness, source })?,
        Err(_elapsed) => {
            // The cancelled `wait_with_output` future drops the `Child` it
            // consumed; `kill_on_drop` then reaps the overrunning child.
            return Err(HarnessError::Timeout {
                harness,
                timeout_ms: spawn.timeout.as_millis() as u64,
            });
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HarnessError::NonZeroExit {
            harness,
            code: output.status.code(),
            stderr: stderr.chars().take(2000).collect(),
        });
    }
    Ok(output)
}

/// Symlink every entry of `from` into `into`, skipping names in `skip`.
///
/// Was byte-identical in the `agy` and `muse` adapters, each building a
/// private home directory that mirrors the real one minus the files it must
/// override. The `muse` copy's own doc comment said "same helper shape as the
/// `agy` adapter's", which is the point at which it should have moved here.
pub(crate) fn link_entries(
    from: &std::path::Path,
    into: &std::path::Path,
    skip: &[&str],
) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if skip.iter().any(|s| std::ffi::OsStr::new(s) == name) {
            continue;
        }
        let target = into.join(&name);
        if target.exists() {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), target)?;
    }
    Ok(())
}
