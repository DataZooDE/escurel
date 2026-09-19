//! The Muse Code CLI harness adapter (#451).
//!
//! **Why this exists now and did not before.** escurel#451 recorded, against
//! Muse Code 1.0.1, that a `muse` harness *could not be built*: the
//! `Harness` contract requires every escurel effect to flow through the
//! agent's own `/mcp` tool calls, and 1.0.1 was not an MCP client at all — no
//! `mcp` subcommand, no `--mcp-config`, no MCP entry in `~/.config/muse/`,
//! and zero occurrences of `mcpServers` in the binary. The negative was
//! deliberately recorded against a version, and Muse Code 1.1.1 changes it:
//! there is a `muse mcp login/logout` subcommand whose own help names
//! "a streamable-HTTP entry under `mcpServers` in settings.json", and the
//! binary carries the machinery to match.
//!
//! So [`MuseHarness`] is a real [`crate::Harness`]: it drives `muse exec`
//! headless as an isolated, timed, kill-on-drop subprocess, registers the
//! escurel gateway as a streamable-HTTP MCP server in a **per-run private
//! config directory** with the packaged scoped bearer, and parses the
//! `--json` JSONL event stream into a [`HarnessOutcome`].
//!
//! Per the contract the adapter performs **no** escurel writes of its own. It
//! is process management + invocation construction + outcome capture.
//!
//! Three decisions are worth reading before changing anything here.
//!
//! **1. The prompt goes in a file, not in argv.** Linux caps a single argv
//! string at 32 pages (`MAX_ARG_STRLEN`) independently of the 2 MB total
//! `ARG_MAX`, and a packaged transcript over ~128 KB then fails to spawn with
//! `E2BIG` — which left an event permanently undispatchable when the Claude
//! adapter hit it. `muse exec` offers `--prompt-file`, so the packaged
//! instructions + input are written to a per-run tempfile.
//!
//! **2. A narrowed tool surface is REFUSED.** `muse exec` has no MCP
//! tool allow-list: `--permission-profile` selects a *named profile from
//! settings*, not an ad-hoc list of tool ids. A run packaged under
//! [`REVIEW_TOOLS`] would therefore reach the model with `update_page`
//! available, and a review skill would commit instead of drafting. Same
//! reasoning, same refusal, as the `agy` adapter — and the same remedy: use
//! `gemini` or `claude` for those runs.
//!
//! **3. The config directory is private per run.** Muse reads MCP servers
//! from `settings.json` in its config dir, so the adapter builds one
//! containing exactly the escurel entry and points `XDG_CONFIG_HOME` at it.
//! The ambient `~/.config/muse/settings.json` of whatever host runs the
//! runner is never used — an operator's own MCP servers must not appear in an
//! unattended run — while the provider credentials ARE linked through, since
//! the model needs them and they are not escurel's business.
//!
//! [`REVIEW_TOOLS`]: escurel_runner_core::REVIEW_TOOLS

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::TaskContext;

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};

/// The adapter's stable name — the `ESCUREL_RUNNER_HARNESS=muse` selector and
/// the value reported by [`Harness::name`].
const NAME: &str = "muse";

/// The MCP server name the gateway is registered under in `settings.json`.
const MCP_SERVER_NAME: &str = "escurel";

/// Default per-run timeout. A real run makes several `/mcp` round-trips and
/// one or more model steps.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Cap on model steps, so a confused run cannot spin against the gateway for
/// the whole timeout. Generous enough for a fold with several tool calls.
const MAX_MODEL_STEPS: u32 = 24;

/// Adapter that drives the real `muse` CLI as a subprocess.
#[derive(Debug, Clone)]
pub struct MuseHarness {
    bin_path: String,
    /// Optional `--model` (config `ESCUREL_RUNNER_MUSE_MODEL`); `None` lets
    /// `muse` use its configured default.
    model: Option<String>,
    timeout: Duration,
    /// The real config dir to link provider credentials from. `None` in tests.
    real_config_home: Option<std::path::PathBuf>,
}

impl MuseHarness {
    /// Build an adapter that launches the `muse` binary at `bin_path`.
    pub fn new(bin_path: impl Into<String>) -> Self {
        Self {
            bin_path: bin_path.into(),
            model: None,
            timeout: DEFAULT_TIMEOUT,
            real_config_home: std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
                }),
        }
    }

    /// Set the `--model` passed to `muse`. An empty/absent value is ignored.
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model.filter(|m| !m.is_empty());
        self
    }

    /// Override the per-run timeout (tests use a small value).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Point the credential-linking at a specific config dir (tests pass a
    /// tempdir, or `None` to link nothing).
    #[must_use]
    pub fn with_real_config_home(mut self, dir: Option<std::path::PathBuf>) -> Self {
        self.real_config_home = dir;
        self
    }

    /// The `settings.json` muse reads the escurel MCP server from.
    ///
    /// `type: http` is the streamable-HTTP transport `muse mcp`'s own help
    /// documents; the scoped bearer rides in the `Authorization` header, as it
    /// does for every other adapter here.
    fn settings_json(&self, task: &TaskContext) -> String {
        let mut settings = serde_json::json!({
            "schema_version": 1,
            "mcpServers": {
                MCP_SERVER_NAME: {
                    "type": "http",
                    "url": task.mcp_endpoint,
                    "headers": {
                        "Authorization": format!("Bearer {}", task.token_str()),
                    },
                },
            },
        });
        if let Some(model) = &self.model {
            settings["model"] = serde_json::json!(model);
        }
        settings.to_string()
    }

    /// Whether the packaged surface is the FULL one.
    ///
    /// A set comparison, not a length one: a task carrying the right number of
    /// the wrong tools is narrowed too.
    fn surface_is_unnarrowed(task: &TaskContext) -> bool {
        let packaged: std::collections::HashSet<&str> =
            task.allowed_tools.iter().map(String::as_str).collect();
        let full: std::collections::HashSet<&str> =
            escurel_runner_core::ALLOWED_TOOLS.iter().copied().collect();
        packaged == full
    }

    /// The argv for `muse exec`, excluding the binary. Split out so a
    /// deterministic test can assert the invocation without spawning.
    fn build_args(&self, prompt_path: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "exec".to_owned(),
            // Machine-readable JSONL events — the only parseable outcome muse
            // offers, and what `parse_outcome` reads.
            "--json".to_owned(),
            // The prompt is a FILE; see this module's note 1.
            "--prompt-file".to_owned(),
            prompt_path.to_owned(),
            // Unattended: no approval prompts, and no interactive sandbox
            // negotiation. The run's authority is the scoped bearer, not
            // muse's own workspace policy.
            "--approval-mode".to_owned(),
            "never".to_owned(),
            "--disable-approval".to_owned(),
            // The agent must not touch the runner's filesystem or shell: its
            // job is to call escurel tools. Every escurel effect flows
            // through `/mcp`, so the workspace tools are pure risk here.
            "--disable-write".to_owned(),
            "--disable-shell".to_owned(),
            "--disable-web-tools".to_owned(),
            // Don't persist a session log into the private config dir that is
            // about to be deleted, and don't leave one behind if it isn't.
            "--no-session-log".to_owned(),
            // A confused run must not spin against the gateway for the whole
            // timeout.
            "--max-model-steps".to_owned(),
            MAX_MODEL_STEPS.to_string(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        args
    }

    /// What the model is asked to do: the skill body as instructions, then
    /// the packaged input.
    ///
    /// `muse exec` takes ONE prompt — there is no `--append-system-prompt`,
    /// and `--provider echo` reports "provider does not support base
    /// instructions" — so the two halves are concatenated with a heading each
    /// rather than smuggled into a system slot that does not exist.
    fn prompt_text(task: &TaskContext) -> String {
        format!(
            "# Instructions\n\n{}\n\n# Input\n\n{}\n",
            task.instructions, task.input
        )
    }
}

/// Build the private config dir: `<tmp>/muse/settings.json` plus symlinks to
/// the real dir's other entries (credentials), and point `XDG_CONFIG_HOME` at
/// its parent.
fn build_config_home(
    real_config_home: Option<&Path>,
    settings: &str,
) -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("escurel-muse-config-")
        .tempdir()?;
    let muse = dir.path().join("muse");
    std::fs::create_dir_all(&muse)?;

    if let Some(real) = real_config_home {
        // Everything but `settings.json` — the credentials the model needs,
        // never the operator's MCP servers or their session logs.
        crate::harness::link_entries(&real.join("muse"), &muse, &["settings.json"])?;
    }
    std::fs::write(muse.join("settings.json"), settings)?;
    Ok(dir)
}

#[async_trait]
impl Harness for MuseHarness {
    fn name(&self) -> &str {
        NAME
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        if !MuseHarness::surface_is_unnarrowed(task) {
            return Err(HarnessError::Unsupported {
                harness: NAME,
                reason: "this task was packaged with a narrowed tool surface (a skill declaring \
                         autonomy other than `auto`, or a workflow step), and `muse exec` has no \
                         way to enforce one: `--permission-profile` selects a named profile from \
                         settings, not an ad-hoc tool allow-list. Running it here would let a \
                         review skill commit. Use the `gemini` or `claude` harness for those runs"
                    .to_owned(),
            });
        }

        // The scoped bearer lives in a file inside a per-run config dir rather
        // than in argv, so it is never in the process table, and it is removed
        // with the tempdir when the run ends.
        let config_home =
            build_config_home(self.real_config_home.as_deref(), &self.settings_json(task))
                .map_err(|source| HarnessError::Io {
                    harness: NAME,
                    source,
                })?;

        let mut prompt = tempfile::Builder::new()
            .prefix("escurel-muse-prompt-")
            .suffix(".md")
            .tempfile()
            .map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?;
        std::io::Write::write_all(&mut prompt, Self::prompt_text(task).as_bytes()).map_err(
            |source| HarnessError::Io {
                harness: NAME,
                source,
            },
        )?;
        let prompt_path = prompt.path().to_string_lossy().into_owned();

        let output = crate::harness::run_capture(crate::harness::Spawn {
            harness: NAME,
            bin: &self.bin_path,
            args: &self.build_args(&prompt_path),
            envs: vec![("XDG_CONFIG_HOME", config_home.path().into())],
            // muse takes its prompt as a FILE PATH in argv, so stdin stays
            // closed rather than piped-and-unwritten.
            stdin: None,
            timeout: self.timeout,
        })
        .await?;

        parse_outcome(&output.stdout)
    }
}

/// Parse `muse exec --json`'s JSONL event stream into a [`HarnessOutcome`].
///
/// The shapes below were read off a real `muse` 1.1.1 run rather than from a
/// spec, because there is no published one:
///
/// - the run ends with one `payload_type: "run.terminal.<terminal>"` record
///   carrying `payload.terminal` (`completed` / …), `payload.text` (the final
///   assistant text) and `payload.reason` (why, when it is not `completed`);
/// - each completed tool call emits one `payload_type: "tool.result"`;
/// - `task.lifecycle.failed` carries a `payload.event.reason` — useful
///   diagnostics when the run itself still reports completed.
///
/// Non-JSON lines are ignored: `muse` writes human diagnostics ("workspace
/// root: …") to stderr, but a stray line on stdout must not fail the parse of
/// an otherwise good run.
fn parse_outcome(stdout: &[u8]) -> Result<HarnessOutcome, HarnessError> {
    let text = String::from_utf8_lossy(stdout);
    let records: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let payload_type = |r: &serde_json::Value| {
        r.get("payload_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };

    let terminal = records
        .iter()
        .rev()
        .find(|r| payload_type(r).starts_with("run.terminal."));
    let Some(terminal) = terminal else {
        // No terminal record at all: the run did not finish, whatever the exit
        // code said. Reported as a bad outcome rather than silently "ok" with
        // no effects — which is how the #450 class of interop failure looked
        // from the outside.
        return Err(HarnessError::BadOutcome {
            harness: NAME,
            source: serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
        });
    };

    let outcome = terminal
        .get("payload")
        .and_then(|p| p.get("terminal"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let ok = outcome == "completed";

    let summary = terminal
        .get("payload")
        .and_then(|p| p.get("text"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(2000).collect::<String>())
        .or_else(|| {
            terminal
                .get("payload")
                .and_then(|p| p.get("reason"))
                .and_then(serde_json::Value::as_str)
                .map(|r| format!("muse run {outcome}: {r}"))
        })
        .unwrap_or_else(|| format!("muse run {outcome}"));

    let tool_calls = u32::try_from(
        records
            .iter()
            .filter(|r| payload_type(r) == "tool.result")
            .count(),
    )
    .unwrap_or(u32::MAX);

    Ok(HarnessOutcome {
        result_ref: None,
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
    use escurel_runner_core::{ALLOWED_TOOLS, REVIEW_TOOLS, SecretString};

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
    fn name_is_muse() {
        assert_eq!(MuseHarness::new("muse").name(), "muse");
    }

    #[test]
    fn settings_declare_escurel_as_an_http_server_with_the_scoped_bearer() {
        let v: serde_json::Value =
            serde_json::from_str(&MuseHarness::new("muse").settings_json(&task())).unwrap();
        let server = &v["mcpServers"]["escurel"];
        assert_eq!(server["type"], "http", "the streamable-HTTP transport");
        assert_eq!(server["url"], "http://127.0.0.1:8080/mcp");
        assert_eq!(
            server["headers"]["Authorization"], "Bearer scoped-bearer-XYZ",
            "the scoped bearer rides in the Authorization header, not in argv"
        );
    }

    #[test]
    fn the_invocation_is_headless_unattended_and_reads_its_prompt_from_a_file() {
        let args = MuseHarness::new("muse").build_args("/tmp/p.md");
        assert_eq!(args[0], "exec");
        assert!(args.contains(&"--json".to_owned()), "{args:?}");
        // The prompt is a FILE: a packaged transcript over ~128 KB exceeds
        // MAX_ARG_STRLEN and makes spawn fail with E2BIG.
        let i = args
            .iter()
            .position(|a| a == "--prompt-file")
            .expect("flag");
        assert_eq!(args[i + 1], "/tmp/p.md");
        assert!(!args.iter().any(|a| a.contains("INPUTMARK")), "{args:?}");
        for flag in [
            "--disable-approval",
            "--disable-write",
            "--disable-shell",
            "--no-session-log",
        ] {
            assert!(args.contains(&flag.to_owned()), "missing {flag}: {args:?}");
        }
        let i = args
            .iter()
            .position(|a| a == "--approval-mode")
            .expect("flag");
        assert_eq!(args[i + 1], "never");
    }

    /// Both halves of the packaged task reach the model, and by a route that
    /// exists: `muse exec` has no system-prompt slot, so the skill body is
    /// part of the prompt or it is nowhere.
    #[test]
    fn the_prompt_carries_the_instructions_and_the_input() {
        let text = MuseHarness::prompt_text(&task());
        assert!(text.contains("INSTRUCTMARK"), "{text}");
        assert!(text.contains("INPUTMARK"), "{text}");
        assert!(
            text.find("INSTRUCTMARK") < text.find("INPUTMARK"),
            "instructions first: {text}"
        );
    }

    /// The gate this adapter must keep honest, and the reason #451 was filed
    /// as "cannot be built" rather than "not built yet": a review run must
    /// not reach a harness that cannot withhold `update_page`.
    #[tokio::test]
    async fn a_narrowed_surface_is_refused_because_muse_cannot_enforce_one() {
        let err = MuseHarness::new("muse")
            .run(&task_with(REVIEW_TOOLS))
            .await
            .expect_err("a review-packaged task must be refused");
        assert!(
            matches!(err, HarnessError::Unsupported { harness, .. } if harness == "muse"),
            "expected Unsupported, got {err:?}"
        );
        assert!(
            format!("{err}").contains("gemini") || format!("{err}").contains("claude"),
            "the refusal must name a harness that CAN run it: {err}"
        );

        // POSITIVE CONTROL: the same adapter accepts the full surface, so the
        // refusal above is about the narrowing and not a harness that refuses
        // everything. It gets as far as the spawn, which fails because the
        // binary does not exist — Spawn, not Unsupported.
        let err = MuseHarness::new("definitely-not-muse-binary")
            .with_real_config_home(None)
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
            !MuseHarness::surface_is_unnarrowed(&task_with(&swapped)),
            "the right NUMBER of the wrong tools is still narrowed"
        );
        assert!(MuseHarness::surface_is_unnarrowed(&task()));
    }

    /// The JSONL shapes, as emitted by a real `muse` 1.1.1 run.
    #[test]
    fn a_completed_run_is_parsed_with_its_text_and_tool_count() {
        let stream = concat!(
            r#"muse: workspace root: /tmp/x"#,
            "\n",
            r#"{"payload_type":"turn.input.user","payload":{}}"#,
            "\n",
            r#"{"payload_type":"tool.result","payload":{}}"#,
            "\n",
            r#"{"payload_type":"tool.result","payload":{}}"#,
            "\n",
            r#"{"payload_type":"run.terminal.completed","payload":{"terminal":"completed","reason":null,"text":"folded the event"}}"#,
            "\n",
        );
        let outcome = parse_outcome(stream.as_bytes()).expect("parses");
        assert!(outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Ok);
        assert_eq!(outcome.summary, "folded the event");
        assert_eq!(outcome.tool_calls, 2, "one per tool.result");
        // A human diagnostic line on stdout must not fail an otherwise good
        // run — the first line above is exactly what muse prints.
    }

    #[test]
    fn a_failed_run_reports_the_reason_and_is_not_ok() {
        let stream = concat!(
            r#"{"payload_type":"run.terminal.failed","payload":{"terminal":"failed","reason":"invalid run configuration","text":""}}"#,
            "\n",
        );
        let outcome = parse_outcome(stream.as_bytes()).expect("parses");
        assert!(!outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Failed);
        assert!(
            outcome.summary.contains("invalid run configuration"),
            "the reason must survive into the summary: {}",
            outcome.summary
        );
    }

    /// A stream with no terminal record is NOT a success with no effects.
    /// That shape is exactly how the #450 interop failure looked from the
    /// outside — a clean exit and zero turns — and calling it `ok` would make
    /// the runner confirm an effect that never happened.
    #[test]
    fn a_stream_with_no_terminal_record_is_a_bad_outcome() {
        let stream = concat!(
            r#"{"payload_type":"run.lifecycle.started","payload":{}}"#,
            "\n",
        );
        let err = parse_outcome(stream.as_bytes()).expect_err("must not be ok");
        assert!(
            matches!(err, HarnessError::BadOutcome { harness, .. } if harness == "muse"),
            "{err:?}"
        );
    }

    /// The private config dir carries the escurel server and NOT the
    /// operator's own settings, while their credentials still come through.
    #[test]
    fn the_private_config_home_hides_the_operators_settings_and_links_credentials() {
        let real = tempfile::tempdir().expect("tempdir");
        let real_muse = real.path().join("muse");
        std::fs::create_dir_all(&real_muse).expect("mkdir");
        std::fs::write(
            real_muse.join("settings.json"),
            r#"{"mcpServers":{"theirs":{}}}"#,
        )
        .expect("write");
        std::fs::write(real_muse.join("auth.json"), "CREDENTIAL").expect("write");

        let dir = build_config_home(Some(real.path()), r#"{"mcpServers":{"escurel":{}}}"#)
            .expect("build");
        let settings =
            std::fs::read_to_string(dir.path().join("muse/settings.json")).expect("read");
        assert!(settings.contains("escurel"), "{settings}");
        assert!(
            !settings.contains("theirs"),
            "an unattended run must not inherit the operator's MCP servers: {settings}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("muse/auth.json")).expect("read"),
            "CREDENTIAL",
            "the provider credential must still reach the model"
        );
    }
}
