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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// What the run cost, as the harness reported it (workbench backend
    /// P3-4). `None` for a harness that reports nothing (echo, agy, muse,
    /// delegate); the runner sums it across attempts onto `run-finished` and
    /// meters it. `#[serde(default)]` keeps older subprocess harnesses valid.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// The user-facing answer of a chat run (governed chat agent): the
    /// model's final message, untruncated — as distinct from `summary`, which
    /// describes the run for the Runs view. `None` for a harness that has no
    /// final message to offer; the runner then falls back to `summary`.
    /// `#[serde(default)]` keeps older subprocess harnesses valid.
    #[serde(default)]
    pub reply: Option<String>,
}

/// Token usage and cost of one harness invocation.
///
/// Counts are what the harness's own accounting reports: for claude each
/// turn's result envelope `usage` summed over the session (input = fresh +
/// cache-read + cache-creation input tokens; the cost is the last envelope's
/// running `total_cost_usd`), for gemini `usageMetadata` summed over the turns, for codex
/// the `turn.completed` usage summed (its `cached_input_tokens` is a subset of
/// `input_tokens`, not added). `cost_usd` only when the harness prices
/// itself (claude's `total_cost_usd`); `model` when it names one.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Provider-reported total, including token classes outside input/output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billed_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub model: Option<String>,
}

impl Usage {
    /// Fold `other` into `self`: tokens and cost add up (a cost stays `None`
    /// only while nobody reported one), the model is the last one reported.
    pub fn accumulate(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.billed_tokens = match (self.billed_tokens, other.billed_tokens) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
        };
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        if other.model.is_some() {
            self.model = other.model.clone();
        }
    }
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
    /// The run was cancelled while the harness was live (workbench backend
    /// P2-3a): a subprocess was stopped (SIGTERM, a grace, SIGKILL), an
    /// in-process loop returned between turns.
    #[error("harness {harness:?} cancelled")]
    Cancelled {
        /// The adapter name.
        harness: &'static str,
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
    /// A gateway preflight failed before a model request was sent.
    #[error("harness {harness:?} preflight error: {message}")]
    Preflight {
        /// The adapter name.
        harness: &'static str,
        /// The preflight failure.
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

    /// Whether a human's message can be written into a LIVE session of this
    /// harness (#820). When true, the runner hands each run an
    /// [`escurel_runner_core::InjectInbox`] on
    /// [`TaskContext::inject`](escurel_runner_core::TaskContext::inject) and
    /// the adapter feeds what arrives on it to the harness as further user
    /// turns. A harness that cannot (one prompt in, one result out) keeps
    /// the default, and an injection is answered `unsupported`.
    fn supports_inject(&self) -> bool {
        false
    }

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
pub(crate) struct Spawn<'a, 'b> {
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
    /// Every stdout line as it arrives (without its newline), for a live
    /// consumer such as a run transcript. The lines are still accumulated
    /// into [`std::process::Output::stdout`] for the post-exit parsers.
    pub stdout_lines: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// Keep stdin open after `stdin` for further user turns. `None` keeps
    /// the write-then-close default above.
    pub turns: Option<Turns<'a, 'b>>,
    pub timeout: std::time::Duration,
    /// The run's cancel handle (workbench backend P2-3a). On cancel the
    /// child gets SIGTERM, [`escurel_runner_core::Cancel::grace`], then
    /// SIGKILL, and the run reports [`HarnessError::Cancelled`]. `None` =
    /// not cancellable.
    pub cancel: Option<&'a escurel_runner_core::Cancel>,
}

/// A conversation over stdin: the initial `stdin` payload is turn one, each
/// message from `inbox` is one more, and stdin closes once every turn the
/// harness was sent has ended — the EOF a stream-json CLI needs to exit.
pub(crate) struct Turns<'a, 'b> {
    /// Further user turns, encoded and written as they arrive. `None` = no
    /// more turns will come. Closed once the last turn has ended, so a
    /// message sent after that is refused at the registry, not lost here.
    pub inbox: Option<&'a mut escurel_runner_core::InjectSession<'b>>,
    /// One turn's text as the bytes the harness reads (a newline-terminated
    /// stream-json message, a JSON string line, …).
    pub encode: fn(&str) -> Vec<u8>,
    /// Whether a stdout line marks the end of a turn. A harness that never
    /// says (`|_| false`) keeps stdin open until it exits.
    pub turn_ended: fn(&str) -> bool,
}

/// Run `spawn` to completion and return its captured output.
///
/// # Errors
/// [`HarnessError::Spawn`] if the binary will not start, [`HarnessError::Io`]
/// on a stdin or wait failure, [`HarnessError::Timeout`] if it outruns the
/// deadline, and [`HarnessError::NonZeroExit`] (with stderr truncated to 2000
/// characters) if it exits non-zero.
pub(crate) async fn run_capture(
    spawn: Spawn<'_, '_>,
) -> Result<std::process::Output, HarnessError> {
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

    // Write the payload; then drop the handle to send EOF, unless the
    // harness holds a conversation, which keeps it for the turns to come.
    let mut open_stdin = None;
    if let Some(payload) = spawn.stdin {
        let mut stdin = child.stdin.take().ok_or_else(|| HarnessError::Io {
            harness,
            source: std::io::Error::other("stdin was not piped"),
        })?;
        tokio::io::AsyncWriteExt::write_all(&mut stdin, payload)
            .await
            .map_err(|source| HarnessError::Io { harness, source })?;
        if spawn.turns.is_some() {
            open_stdin = Some(stdin);
        } else {
            tokio::io::AsyncWriteExt::shutdown(&mut stdin)
                .await
                .map_err(|source| HarnessError::Io { harness, source })?;
        }
    }
    let (encode, turn_ended, mut inbox) = match spawn.turns {
        Some(t) => (Some(t.encode), Some(t.turn_ended), t.inbox),
        None => (None, None, None),
    };

    // Read both pipes concurrently with the wait: a child that fills a pipe
    // nobody drains would block, and the wait must stay selectable against
    // the deadline and the cancel handle (which `wait_with_output`, owning
    // the child, is not).
    let (ended_tx, mut ended_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let stdout = child.stdout.take().map(|h| {
        let sink = spawn.stdout_lines.clone();
        tokio::spawn(stream_stdout(h, sink, turn_ended, ended_tx))
    });
    let stderr = child.stderr.take().map(|mut h| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut h, &mut buf).await;
            buf
        })
    });
    let cancelled = async {
        match spawn.cancel {
            Some(c) => c.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(cancelled);
    let deadline = tokio::time::sleep(spawn.timeout);
    tokio::pin!(deadline);
    // Turns sent that the harness has not yet ended: the initial payload is
    // the first.
    let mut pending_turns: usize = 1;
    let status = loop {
        tokio::select! {
            // Biased: a buffered turn is written before an ended turn can
            // close stdin under it.
            biased;
            result = child.wait() => {
                break result.map_err(|source| HarnessError::Io { harness, source })?;
            }
            () = &mut deadline => {
                // The `Child` is dropped on return; `kill_on_drop` reaps it.
                return Err(HarnessError::Timeout {
                    harness,
                    timeout_ms: spawn.timeout.as_millis() as u64,
                });
            }
            () = &mut cancelled => {
                let grace = spawn.cancel.map_or(std::time::Duration::ZERO, |c| c.grace);
                stop_child(&mut child, grace).await;
                return Err(HarnessError::Cancelled { harness });
            }
            turn = next_turn(&mut inbox), if open_stdin.is_some() => {
                match (turn, open_stdin.as_mut(), encode) {
                    (Some(text), Some(stdin), Some(encode)) => {
                        tokio::io::AsyncWriteExt::write_all(stdin, &encode(&text))
                            .await
                            .map_err(|source| HarnessError::Io { harness, source })?;
                        pending_turns += 1;
                    }
                    // Nobody can send another turn.
                    _ => inbox = None,
                }
            }
            Some(()) = ended_rx.recv() => {
                pending_turns = pending_turns.saturating_sub(1);
                if pending_turns == 0 {
                    // Close before the EOF: a message accepted in between
                    // is still written as one more turn.
                    let late = inbox.take().map(|i| i.close()).unwrap_or_default();
                    match (open_stdin.as_mut(), encode) {
                        (Some(stdin), Some(encode)) if !late.is_empty() => {
                            for text in &late {
                                tokio::io::AsyncWriteExt::write_all(stdin, &encode(text))
                                    .await
                                    .map_err(|source| HarnessError::Io { harness, source })?;
                            }
                            pending_turns += late.len();
                        }
                        // Dropping the handle is the EOF that ends the session.
                        _ => open_stdin = None,
                    }
                }
            }
        }
    };
    drop(open_stdin);
    let collect = |h: Option<tokio::task::JoinHandle<Vec<u8>>>| async move {
        match h {
            Some(h) => h.await.unwrap_or_default(),
            None => Vec::new(),
        }
    };
    let output = std::process::Output {
        status,
        stdout: collect(stdout).await,
        stderr: collect(stderr).await,
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

/// The next turn from `inbox`; pends forever when there is none.
async fn next_turn(
    inbox: &mut Option<&mut escurel_runner_core::InjectSession<'_>>,
) -> Option<String> {
    match inbox {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Read stdout line by line: accumulate every byte for the post-exit
/// parsers, hand each line to `sink`, and signal `ended` for each line that
/// closes a turn.
async fn stream_stdout(
    pipe: tokio::process::ChildStdout,
    sink: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    turn_ended: Option<fn(&str) -> bool>,
    ended: tokio::sync::mpsc::UnboundedSender<()>,
) -> Vec<u8> {
    let mut reader = tokio::io::BufReader::new(pipe);
    let mut all = Vec::new();
    loop {
        let start = all.len();
        match tokio::io::AsyncBufReadExt::read_until(&mut reader, b'\n', &mut all).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&all[start..]);
        let line = line.trim_end_matches(['\n', '\r']);
        if turn_ended.is_some_and(|ended_by| ended_by(line)) {
            let _ = ended.send(());
        }
        if let Some(sink) = &sink {
            let _ = sink.send(line.to_owned());
        }
    }
    all
}

/// Stop a cancelled child: SIGTERM so a harness that cleans up (a CLI
/// flushing its session, an MCP client closing) may, wait `grace`, then
/// SIGKILL. A child that ignores SIGTERM is still gone after the grace.
async fn stop_child(child: &mut tokio::process::Child, grace: std::time::Duration) {
    if let Some(pid) = child.id() {
        // Through the `kill` utility rather than `libc::kill`: the workspace
        // forbids `unsafe`, tokio's own `start_kill` is SIGKILL-only, and a
        // fork per cancel is nothing. `child.id()` is `Some` only while the
        // child is live, so the pid is ours and unreaped.
        let _ = tokio::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    if tokio::time::timeout(grace, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
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

#[cfg(test)]
mod cancel_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Vec<String> {
        vec!["-c".to_owned(), script.to_owned()]
    }

    #[tokio::test]
    async fn a_cancelled_child_is_stopped_and_the_run_reports_cancelled() {
        let cancel = escurel_runner_core::Cancel::new(Duration::from_secs(2));
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            c.cancel();
        });
        let started = Instant::now();
        let args = sh("sleep 30");
        let result = run_capture(Spawn {
            harness: "test",
            bin: "sh",
            args: &args,
            envs: Vec::new(),
            stdin: None,
            stdout_lines: None,
            turns: None,
            timeout: Duration::from_secs(60),
            cancel: Some(&cancel),
        })
        .await;
        assert!(
            matches!(result, Err(HarnessError::Cancelled { harness: "test" })),
            "{result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stopped, not waited out"
        );
    }

    #[tokio::test]
    async fn a_child_that_ignores_sigterm_is_killed_after_the_grace() {
        let cancel = escurel_runner_core::Cancel::new(Duration::from_millis(300));
        let c = cancel.clone();
        // After the shell has installed its trap; a SIGTERM before that
        // would simply end it.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            c.cancel();
        });
        let started = Instant::now();
        let args = sh("trap '' TERM; sleep 30");
        let result = run_capture(Spawn {
            harness: "test",
            bin: "sh",
            args: &args,
            envs: Vec::new(),
            stdin: None,
            stdout_lines: None,
            turns: None,
            timeout: Duration::from_secs(60),
            cancel: Some(&cancel),
        })
        .await;
        assert!(
            matches!(result, Err(HarnessError::Cancelled { .. })),
            "{result:?}"
        );
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(650) && took < Duration::from_secs(5),
            "{took:?}"
        );
    }

    #[tokio::test]
    async fn an_uncancelled_run_still_completes_normally() {
        let args = sh("printf hello");
        let output = run_capture(Spawn {
            harness: "test",
            bin: "sh",
            args: &args,
            envs: Vec::new(),
            stdin: None,
            stdout_lines: None,
            turns: None,
            timeout: Duration::from_secs(10),
            cancel: None,
        })
        .await
        .expect("runs");
        assert_eq!(output.stdout, b"hello");
    }

    #[tokio::test]
    async fn stdout_lines_stream_to_the_sink_and_are_still_captured() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let args = sh("printf 'one\\ntwo\\nthree'");
        let output = run_capture(Spawn {
            harness: "test",
            bin: "sh",
            args: &args,
            envs: Vec::new(),
            stdin: None,
            stdout_lines: Some(tx),
            turns: None,
            timeout: Duration::from_secs(10),
            cancel: None,
        })
        .await
        .expect("runs");
        assert_eq!(output.stdout, b"one\ntwo\nthree");
        let mut lines = Vec::new();
        while let Ok(l) = rx.try_recv() {
            lines.push(l);
        }
        assert_eq!(lines, ["one", "two", "three"]);
    }

    fn line(text: &str) -> Vec<u8> {
        format!("{text}\n").into_bytes()
    }

    /// The shape of a stream-json session: the child answers each stdin
    /// line with a `done` line and only exits at EOF. A queued turn is
    /// written, and stdin closes once both turns have ended.
    #[tokio::test]
    async fn a_conversation_keeps_stdin_open_until_every_turn_has_ended() {
        let (tx, inbox) = escurel_runner_core::InjectInbox::channel();
        assert!(tx.send("steer"));
        let mut rx = inbox.open_session().await;
        let args = sh("while read -r l; do echo \"got $l\"; echo done; done; echo eof");
        let output = run_capture(Spawn {
            harness: "test",
            bin: "sh",
            args: &args,
            envs: Vec::new(),
            stdin: Some(b"first\n"),
            stdout_lines: None,
            turns: Some(Turns {
                inbox: Some(&mut rx),
                encode: line,
                turn_ended: |l| l == "done",
            }),
            timeout: Duration::from_secs(10),
            cancel: None,
        })
        .await
        .expect("the child saw EOF and exited");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "got first\ndone\ngot steer\ndone\neof\n"
        );
        assert!(!tx.send("late"), "the session closed with its last turn");
    }

    /// A harness that never marks a turn ended keeps stdin until it exits;
    /// turns injected while it runs reach it.
    #[tokio::test]
    async fn turns_injected_mid_run_reach_a_harness_that_reads_until_it_is_done() {
        let (tx, inbox) = escurel_runner_core::InjectInbox::channel();
        let mut rx = inbox.open_session().await;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = tx.send("mid-run");
            // Held: the harness, not the sender, decides when it is done.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let args = sh("read -r a; read -r b; echo \"$a+$b\"");
        let output = run_capture(Spawn {
            harness: "test",
            bin: "sh",
            args: &args,
            envs: Vec::new(),
            stdin: Some(b"task\n"),
            stdout_lines: None,
            turns: Some(Turns {
                inbox: Some(&mut rx),
                encode: line,
                turn_ended: |_| false,
            }),
            timeout: Duration::from_secs(10),
            cancel: None,
        })
        .await
        .expect("runs");
        assert_eq!(output.stdout, b"task+mid-run\n");
    }
}
