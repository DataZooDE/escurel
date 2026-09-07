//! The Antigravity (`agy`) CLI harness adapter.
//!
//! [`AgyHarness`] drives the `agy` headless CLI as an isolated, timed,
//! kill-on-drop subprocess, registering the escurel gateway as an **HTTP MCP
//! server** with the packaged scoped bearer, and parsing the NDJSON
//! `{"event":"result",...}` frame into a [`HarnessOutcome`].
//!
//! Like every adapter it performs **no** escurel writes: every escurel effect
//! is a tool the model chose over `/mcp` under the scoped token.
//!
//! # Two things about `agy` that shape this adapter
//!
//! **It only runs `autonomy: auto` skills, and it says so.** `agy` exposes
//! MCP tools through one generic `call_mcp_tool`, and its CLI has no
//! `--allowedTools` equivalent — the permission surface is interactive review
//! or `--dangerously-skip-permissions`, nothing in between. So an adapter
//! cannot narrow the tool surface the way `claude` / `codex` / `gemini` do,
//! and a run packaged under [`REVIEW_TOOLS`] would reach `update_page`
//! anyway. That would make the autonomy gate advisory, which is the one
//! direction it must not fail in. [`AgyHarness::run`] therefore REFUSES a
//! task whose packaged surface is not the full committing one, rather than
//! running it with a surface nobody enforces.
//!
//! [`REVIEW_TOOLS`]: escurel_runner_core::REVIEW_TOOLS
//!
//! **Its MCP config is global, and `HOME` is the only lever.** `agy` reads
//! `~/.gemini/config/mcp_config.json`; there is no per-invocation config flag
//! (`--gemini_dir` exists but the `mcp` subcommands ignore it). Writing the
//! scoped bearer into the operator's own config would leak it into a file
//! outliving the run and would race every concurrent run. So each run gets a
//! private `HOME` whose `.gemini` is a symlink farm over the real one —
//! credentials shared, `mcp_config.json` this run's own — removed when the
//! run ends.
//!
//! The `agy` binary path is configurable (`ESCUREL_RUNNER_AGY_BIN`, default
//! `agy`) so a deterministic test can point it at a stub executable that
//! mimics the CLI's I/O contract, exercising the whole invocation-build and
//! parse path without burning model quota. The live end-to-end test (real
//! `agy` against a real `/mcp`) runs on demand behind `#[ignore]`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::{ALLOWED_TOOLS, TaskContext};

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};

/// The adapter's stable name — the `ESCUREL_RUNNER_HARNESS=agy` selector and
/// the value reported by [`Harness::name`].
const NAME: &str = "agy";

/// The MCP server name the gateway is registered under.
const MCP_SERVER_NAME: &str = "escurel";

/// Default per-run timeout, matching the other LLM adapters.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Adapter that drives the real `agy` CLI as a subprocess.
#[derive(Debug, Clone)]
pub struct AgyHarness {
    /// Path to the `agy` binary to spawn (default `agy`).
    bin_path: String,
    /// Optional `--model`; `None` lets `agy` follow its own configuration —
    /// which for `agy` means whatever `/model` last selected interactively,
    /// so a deployment should pin it.
    model: Option<String>,
    /// The home directory holding `agy`'s credentials (`~/.gemini`). Every
    /// run links its own `HOME` over this one.
    real_home: Option<PathBuf>,
    /// Per-run timeout; the child is killed (kill-on-drop) if it overruns.
    timeout: Duration,
}

impl AgyHarness {
    /// Build an adapter that launches the `agy` binary at `bin_path`.
    pub fn new(bin_path: impl Into<String>) -> Self {
        Self {
            bin_path: bin_path.into(),
            model: None,
            real_home: std::env::var_os("HOME").map(PathBuf::from),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Set the `--model` passed to `agy`. An empty/absent value is ignored.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model.filter(|m| !m.is_empty());
        self
    }

    /// Point the adapter at the home holding `agy`'s credentials
    /// (`ESCUREL_RUNNER_AGY_HOME`); defaults to the process's own `HOME`.
    pub fn with_home(mut self, home: Option<String>) -> Self {
        if let Some(h) = home.filter(|h| !h.is_empty()) {
            self.real_home = Some(PathBuf::from(h));
        }
        self
    }

    /// Override the per-run timeout (tests use a small value).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The escurel MCP server entry, in the shape `agy` stores it.
    ///
    /// `serverUrl` + `headers`, not claude's `type`/`url` — a different CLI
    /// with a different file format, which is exactly why this is one
    /// function and not a shared constant.
    fn mcp_config_json(task: &TaskContext) -> String {
        serde_json::json!({
            "mcpServers": {
                MCP_SERVER_NAME: {
                    "disabled": false,
                    "serverUrl": task.mcp_endpoint,
                    "headers": {
                        "Authorization": format!("Bearer {}", task.token_str()),
                    },
                },
            },
        })
        .to_string()
    }

    /// Whether the packaged surface is the full committing one.
    ///
    /// The adapter cannot narrow `agy`'s tools, so it may only run a task
    /// that was allowed everything anyway. Compared as a SET against
    /// [`ALLOWED_TOOLS`]: a task packaged with fewer tools (a `review` run,
    /// a workflow step) is one this harness would silently widen.
    fn surface_is_unnarrowed(task: &TaskContext) -> bool {
        task.allowed_tools.len() == ALLOWED_TOOLS.len()
            && ALLOWED_TOOLS
                .iter()
                .all(|t| task.allowed_tools.iter().any(|a| a == t))
    }

    /// Assemble the `agy` argv (excluding the binary itself).
    ///
    /// `--print=` carries no prompt: the prompt travels as one NDJSON `user`
    /// message on stdin (`--input-format stream-json`). Linux caps a SINGLE
    /// argv string at 32 pages independently of `ARG_MAX`, so a packaged
    /// transcript over ~128 KB in argv makes spawn fail with E2BIG and the
    /// event permanently undispatchable — measured on the claude adapter,
    /// against a real 220 KB meeting transcript.
    fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "--input-format".to_owned(),
            "stream-json".to_owned(),
            // stream-json input REQUIRES stream-json output.
            "--output-format".to_owned(),
            "stream-json".to_owned(),
            // Unattended: there is no one to answer a permission prompt, and
            // an unanswered one burns the whole run timeout.
            "--dangerously-skip-permissions".to_owned(),
            // Terminal restrictions stay on. The agent's job is `/mcp` tool
            // calls; it has no business running commands on the runner host.
            "--sandbox".to_owned(),
            // The packaged instructions are UNTRUSTED text — a skill body
            // plus an event payload that arrived from outside. Slash-command
            // and skill expansion turns a leading `/` in that text into a
            // local command, so it is off.
            "--disable-slash-commands".to_owned(),
            "--print-timeout".to_owned(),
            format!("{}s", self.timeout.as_secs()),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        // MUST be one token and last: `--print` is a Go string flag, so
        // `--print --output-format` would make `--output-format` the prompt.
        args.push("--print=".to_owned());
        args
    }

    /// The one NDJSON message written to the child's stdin.
    ///
    /// Instructions and input are joined into a single user turn because
    /// `agy` has no system-prompt flag: everything the model must read has
    /// to arrive as the message.
    fn stdin_message(task: &TaskContext) -> String {
        let text = format!("{}\n\n{}", task.instructions, task.input);
        format!(
            "{}\n",
            serde_json::json!({
                "event": "user",
                "message": {
                    "role": "user",
                    "content": [{ "type": "text", "text": text }],
                },
            })
        )
    }
}

/// Build the run's private `HOME`, sharing `agy`'s credentials and carrying
/// this run's own `mcp_config.json`.
///
/// Everything in the real `~/.gemini` is symlinked in except `config`, which
/// is rebuilt entry by entry so the one file this run must own — and only
/// that file — is a real file. A missing real home is not an error: `agy`
/// will report its own unauthenticated state far more usefully than a
/// guess here would.
fn build_home(real_home: Option<&Path>, mcp_config: &str) -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("escurel-agy-home-")
        .tempdir()?;
    let gemini = dir.path().join(".gemini");
    let config = gemini.join("config");
    std::fs::create_dir_all(&config)?;

    if let Some(real) = real_home {
        link_entries(&real.join(".gemini"), &gemini, &["config"])?;
        link_entries(&real.join(".gemini/config"), &config, &["mcp_config.json"])?;
    }
    std::fs::write(config.join("mcp_config.json"), mcp_config)?;
    Ok(dir)
}

/// Symlink every entry of `from` into `into`, skipping `skip` and anything
/// already there. A `from` that does not exist links nothing.
fn link_entries(from: &Path, into: &Path, skip: &[&str]) -> std::io::Result<()> {
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

#[async_trait]
impl Harness for AgyHarness {
    fn name(&self) -> &str {
        NAME
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        if !AgyHarness::surface_is_unnarrowed(task) {
            return Err(HarnessError::Unsupported {
                harness: NAME,
                reason: "this task was packaged with a narrowed tool surface (a skill declaring \
                         autonomy other than `auto`, or a workflow step), and `agy` has no way \
                         to enforce one: its MCP tools all arrive through `call_mcp_tool` and \
                         its CLI offers no allow-list. Running it here would let a review skill \
                         commit. Use the `gemini` harness for those runs"
                    .to_owned(),
            });
        }

        // The scoped bearer lives in a file inside a per-run HOME rather than
        // in argv, so it is never in the process table, and is removed with
        // the tempdir when the run ends.
        let home = build_home(self.real_home.as_deref(), &Self::mcp_config_json(task)).map_err(
            |source| HarnessError::Io {
                harness: NAME,
                source,
            },
        )?;

        let mut child = tokio::process::Command::new(&self.bin_path)
            .args(self.build_args())
            .env("HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| HarnessError::Spawn {
                harness: NAME,
                path: self.bin_path.clone(),
                source,
            })?;

        {
            let mut stdin = child.stdin.take().ok_or_else(|| HarnessError::Io {
                harness: NAME,
                source: std::io::Error::other("stdin was not piped"),
            })?;
            tokio::io::AsyncWriteExt::write_all(&mut stdin, Self::stdin_message(task).as_bytes())
                .await
                .map_err(|source| HarnessError::Io {
                    harness: NAME,
                    source,
                })?;
            // EOF, or `agy` waits for a second turn until the run times out.
            tokio::io::AsyncWriteExt::shutdown(&mut stdin)
                .await
                .map_err(|source| HarnessError::Io {
                    harness: NAME,
                    source,
                })?;
        }

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?,
            Err(_elapsed) => {
                return Err(HarnessError::Timeout {
                    harness: NAME,
                    timeout_ms: self.timeout.as_millis() as u64,
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(HarnessError::NonZeroExit {
                harness: NAME,
                code: output.status.code(),
                stderr: stderr.chars().take(2000).collect(),
            });
        }

        parse_outcome(&output.stdout)
    }
}

/// Parse `agy --output-format stream-json` NDJSON into a [`HarnessOutcome`].
///
/// Only the final `{"event":"result","result":{…}}` frame is read; the
/// `step_update` frames before it are progress. The frame carries
/// `status` (`SUCCESS`/`ERROR`), `response` (the final assistant text),
/// `error` when it failed, and `num_turns`.
///
/// `produced_instance` is `None`: the envelope does not name the page the
/// model wrote, and the runner's reconciler reads that back from the gateway
/// rather than believing the harness.
fn parse_outcome(stdout: &[u8]) -> Result<HarnessOutcome, HarnessError> {
    let text = String::from_utf8_lossy(stdout);
    let result = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("event").and_then(serde_json::Value::as_str) == Some("result"))
        .next_back()
        .and_then(|v| v.get("result").cloned());

    // A stream that ended without a result frame is not an empty success:
    // the run was cut off, and reporting `ok` would mark the event processed
    // for work that may never have happened.
    let Some(result) = result else {
        return Err(HarnessError::BadOutcome {
            harness: NAME,
            source: serde_json::from_str::<serde_json::Value>("")
                .expect_err("the empty string is not valid JSON"),
        });
    };

    let status = result
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let ok = status == "SUCCESS";

    let summary = result
        .get(if ok { "response" } else { "error" })
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(2000).collect::<String>())
        .unwrap_or_else(|| format!("agy run: {status}"));

    let tool_calls = result
        .get("num_turns")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;

    Ok(HarnessOutcome {
        ok,
        status: if ok {
            HarnessStatus::Ok
        } else {
            HarnessStatus::Failed
        },
        summary,
        tool_calls,
        produced_instance: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use escurel_runner_core::{REVIEW_TOOLS, SecretString};

    fn task_with(tools: &[&str]) -> TaskContext {
        TaskContext::for_test(
            "INSTRUCTMARK skill body".to_owned(),
            "INPUTMARK fold this".to_owned(),
            "http://127.0.0.1:8080/mcp".to_owned(),
            tools.iter().map(|t| (*t).to_owned()).collect(),
            SecretString::from("scoped-bearer-XYZ".to_owned()),
        )
    }

    fn task() -> TaskContext {
        task_with(ALLOWED_TOOLS)
    }

    #[test]
    fn name_is_agy() {
        assert_eq!(AgyHarness::new("agy").name(), "agy");
    }

    #[test]
    fn mcp_config_declares_escurel_with_the_scoped_bearer() {
        let v: serde_json::Value =
            serde_json::from_str(&AgyHarness::mcp_config_json(&task())).unwrap();
        let server = &v["mcpServers"]["escurel"];
        assert_eq!(server["serverUrl"], "http://127.0.0.1:8080/mcp");
        assert_eq!(server["disabled"], false);
        assert_eq!(
            server["headers"]["Authorization"], "Bearer scoped-bearer-XYZ",
            "the scoped bearer rides in the Authorization header"
        );
    }

    /// The gate this adapter exists to keep honest. A review run reaches
    /// `agy` with `create_draft` and no `update_page`; `agy` would offer the
    /// model every tool the token can reach regardless, so the run must be
    /// refused rather than widened.
    #[tokio::test]
    async fn a_narrowed_surface_is_refused_because_agy_cannot_enforce_one() {
        let review = task_with(REVIEW_TOOLS);
        let err = AgyHarness::new("agy")
            .run(&review)
            .await
            .expect_err("a review-packaged task must be refused");
        assert!(
            matches!(err, HarnessError::Unsupported { harness, .. } if harness == "agy"),
            "expected Unsupported, got {err:?}"
        );
        assert!(
            format!("{err}").contains("gemini"),
            "the refusal must name the harness that CAN run it: {err}"
        );

        // POSITIVE CONTROL: the same adapter accepts the full surface, so
        // the refusal above is the narrowing and not a harness that refuses
        // everything. It gets as far as spawning, which fails here because
        // `definitely-not-agy` is not a binary — a Spawn error, not
        // Unsupported.
        let err = AgyHarness::new("definitely-not-agy-binary")
            .run(&task())
            .await
            .expect_err("no such binary");
        assert!(
            matches!(err, HarnessError::Spawn { .. }),
            "control: an auto-packaged task must reach the spawn, got {err:?}"
        );
    }

    #[test]
    fn the_surface_check_is_a_set_comparison_not_a_length_one() {
        let mut swapped: Vec<&str> = ALLOWED_TOOLS.to_vec();
        swapped[0] = "delete_everything";
        assert!(
            !AgyHarness::surface_is_unnarrowed(&task_with(&swapped)),
            "same length, different membership: must not pass"
        );
        let mut reordered: Vec<&str> = ALLOWED_TOOLS.to_vec();
        reordered.reverse();
        assert!(
            AgyHarness::surface_is_unnarrowed(&task_with(&reordered)),
            "control: order must not matter"
        );
    }

    /// Linux caps a SINGLE argv string at 32 pages, independently of the 2 MB
    /// total `ARG_MAX`. A 220 KB transcript in argv makes spawn fail with
    /// E2BIG and the event permanently undispatchable.
    const MAX_ARG_STRLEN: usize = 32 * 4096;

    #[test]
    fn the_prompt_is_never_in_argv() {
        let args = AgyHarness::new("agy").build_args();
        assert!(
            args.iter().all(|a| a.len() < MAX_ARG_STRLEN),
            "no argv element may approach the per-argument limit: {args:?}"
        );
        assert!(
            args.contains(&"--print=".to_owned()),
            "--print carries no prompt; the prompt is on stdin: {args:?}"
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some("--print="),
            "--print must be the LAST token: it is a Go string flag, so a \
             following flag would become the prompt"
        );

        // …and what IS the prompt carries both halves, since agy has no
        // system-prompt flag to put the instructions in.
        let msg: serde_json::Value =
            serde_json::from_str(AgyHarness::stdin_message(&task()).trim()).unwrap();
        assert_eq!(msg["event"], "user");
        let text = msg["message"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("INSTRUCTMARK skill body"), "{text}");
        assert!(text.contains("INPUTMARK fold this"), "{text}");
    }

    #[test]
    fn argv_is_unattended_sandboxed_and_streaming() {
        let args = AgyHarness::new("agy")
            .with_model(Some("gemini-3.8-flash-high".to_owned()))
            .with_timeout(Duration::from_secs(120))
            .build_args();
        let after = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(after("--input-format"), Some("stream-json".to_owned()));
        assert_eq!(after("--output-format"), Some("stream-json".to_owned()));
        assert_eq!(after("--model"), Some("gemini-3.8-flash-high".to_owned()));
        assert_eq!(after("--print-timeout"), Some("120s".to_owned()));
        assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(args.iter().any(|a| a == "--sandbox"));
        assert!(
            args.iter().any(|a| a == "--disable-slash-commands"),
            "packaged instructions are untrusted text; a leading `/` must not \
             become a local command"
        );
    }

    #[test]
    fn model_is_omitted_when_unset() {
        assert!(
            !AgyHarness::new("agy")
                .build_args()
                .iter()
                .any(|a| a == "--model")
        );
    }

    /// The run's `HOME` must carry this run's MCP config and the real home's
    /// credentials, and must NOT write into the real home.
    #[test]
    fn the_private_home_shares_credentials_and_owns_only_the_mcp_config() {
        let real = tempfile::tempdir().unwrap();
        let gemini = real.path().join(".gemini");
        std::fs::create_dir_all(gemini.join("config")).unwrap();
        std::fs::write(gemini.join("oauth_creds.json"), "CREDS").unwrap();
        std::fs::write(gemini.join("config/config.json"), "SETTINGS").unwrap();
        std::fs::write(gemini.join("config/mcp_config.json"), "OPERATORS-OWN").unwrap();

        let home = build_home(Some(real.path()), &AgyHarness::mcp_config_json(&task())).unwrap();
        let cfg = home.path().join(".gemini/config");

        // Credentials and other settings are reachable through the private
        // home, or `agy` runs unauthenticated.
        assert_eq!(
            std::fs::read_to_string(home.path().join(".gemini/oauth_creds.json")).unwrap(),
            "CREDS"
        );
        assert_eq!(
            std::fs::read_to_string(cfg.join("config.json")).unwrap(),
            "SETTINGS"
        );

        // The MCP config is this run's own…
        let mine = std::fs::read_to_string(cfg.join("mcp_config.json")).unwrap();
        assert!(mine.contains("scoped-bearer-XYZ"), "{mine}");
        assert!(
            !std::fs::symlink_metadata(cfg.join("mcp_config.json"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "it must be a real file, or the write below would follow the link"
        );

        // …and the operator's is untouched. A scoped bearer written into the
        // real config would outlive the run and race every concurrent one.
        assert_eq!(
            std::fs::read_to_string(gemini.join("config/mcp_config.json")).unwrap(),
            "OPERATORS-OWN"
        );
    }

    #[test]
    fn a_missing_real_home_still_yields_a_usable_config() {
        let home = build_home(
            Some(Path::new("/nonexistent-home-for-a-test")),
            &AgyHarness::mcp_config_json(&task()),
        )
        .expect("a missing credential home is agy's problem to report, not a spawn failure");
        assert!(
            std::fs::read_to_string(home.path().join(".gemini/config/mcp_config.json"))
                .unwrap()
                .contains("escurel")
        );
    }

    #[test]
    fn parses_the_result_frame_after_the_progress_frames() {
        let stdout = br#"{"event":"init","init":{"model":"m"}}
{"event":"step_update","step_update":{"state":"ACTIVE","text_delta":"work"}}
{"event":"result","result":{"status":"SUCCESS","response":"folded the event","num_turns":4}}
"#;
        let outcome = parse_outcome(stdout).expect("parse");
        assert!(outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Ok);
        assert_eq!(outcome.summary, "folded the event");
        assert_eq!(outcome.tool_calls, 4);
    }

    #[test]
    fn an_error_frame_carries_its_reason() {
        let stdout = br#"{"event":"result","result":{"status":"ERROR","response":"","error":"the model request failed","num_turns":1}}"#;
        let outcome = parse_outcome(stdout).expect("parse");
        assert!(!outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Failed);
        assert_eq!(
            outcome.summary, "the model request failed",
            "the upstream's own words: a bare status has cost real debugging time"
        );
    }

    /// A stream that stopped before its result frame is not a quiet success.
    /// Reporting `ok` would mark the event processed for work that may never
    /// have happened.
    #[test]
    fn a_truncated_stream_is_an_error_not_an_empty_success() {
        let stdout = br#"{"event":"init","init":{}}
{"event":"step_update","step_update":{"state":"ACTIVE"}}
"#;
        let err = parse_outcome(stdout).expect_err("must not parse as ok");
        assert!(matches!(err, HarnessError::BadOutcome { .. }), "{err:?}");

        // POSITIVE CONTROL: the same parser on the same stream WITH its
        // result frame succeeds, so the error above is the truncation.
        let whole = br#"{"event":"init","init":{}}
{"event":"result","result":{"status":"SUCCESS","response":"done","num_turns":1}}
"#;
        assert!(parse_outcome(whole).expect("parse").ok);
    }
}
