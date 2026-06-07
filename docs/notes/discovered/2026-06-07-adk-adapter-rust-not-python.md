# ADK harness adapter: adk-rust by-path, not in-tree Python (#154)

## Symptom

Issue #154 ("[runner 10/14] Google ADK adapter") specced *"a thin **Python**
ADK runner script (shipped with the crate) that builds an `Agent` … via ADK's
`MCPToolset` (streamable-HTTP) … → behind a feature flag."* Following that
literally fails here:

- Python `google.adk` is **not installed** on the target machine, so the
  "shipped Python runner script" has nothing to run against.
- The chosen ADK example is DataZoo's **adk-rust** template
  (`https://github.com/DataZooDE/datazoo-agent-template`). Its `Cargo.toml`
  pulls `adk-rust = "0.9"` **plus a bundled `duckdb`** — a large native
  dependency tree the template deliberately keeps in its **own standalone
  workspace** ("adk-rust + DuckDB pull large trees we keep scoped to this
  crate").

Vendoring adk-rust into escurel's workspace would bloat *every* build and
break the runner crates' deliberate independence: `escurel-runner-core` /
`escurel-runner-harness` depend only on `escurel-client` + `escurel-types`,
never on `escurel-server` / `escurel-index` (and now never on `adk-rust`).

## Decision / fix

The ADK adapter follows the **exact same spawn-an-external-binary-by-path**
pattern as the Claude (#152) and Codex (#153) adapters — it does **not**
vendor adk-rust:

- `AdkHarness` spawns an **external adk-rust runner binary** at a configurable
  path (`ESCUREL_RUNNER_ADK_BIN`, default `datazoo-agent-adk-runner` — no such
  binary on `PATH`, so a deployment must point it at its built runner).
- I/O contract: a **token-less `AdkTask` JSON on the child's stdin**
  (`{instructions, input, mcp_endpoint, allowed_tools}`) + the scoped bearer
  delivered **out-of-band** via the `ESCUREL_MCP_BEARER` env var (mirroring
  the Codex adapter — keeps the token out of argv/process-table/payload).
  Optional `LLM_MODEL` (from `ESCUREL_RUNNER_ADK_MODEL`) + `LLM_PROVIDER` /
  provider key pass through. The runner prints a `HarnessOutcome` JSON on
  stdout. The adapter performs **no** escurel writes.
- The heavy adk-rust runtime lives in that **external** binary (built from the
  template), mirroring how the template isolates adk-rust in a standalone
  workspace. **No cargo feature flag is needed** — the "behind a feature flag"
  wording was for the in-tree Python runtime we don't have; this adapter is
  just a subprocess spawn, so there is no heavy dep to gate. (Documented in
  the `AdkHarness` module doc-comment.)

## How the DoD stays real (no mocks)

- **Always-on** `crates/escurel-runner/tests/adk_end_to_end.rs`: a real
  `EscurelProcess` gateway + real `FixtureBuilder` data + a real
  `capture_event`; a **real `bash` + `curl`/`jq` adk-runner stub** (the
  deterministic analogue of the template's `StaticBrain` — only the tool
  *choice* is scripted; every `/mcp` call is a real JSON-RPC round-trip to the
  real gateway) performs the real `expand` → `update_page` → `assign_event`
  fold under the scoped bearer, driven through `AdkHarness` by the real
  `escurel-runner`. The test asserts the event is `processed` (real
  `list_events`) and the instance body carries the runner's `adk-runner folded
  event` marker (real `expand`). That marker is the load-bearing
  discriminator: drop the `adk` arm from `build_harness` and the echo fallback
  runs, whose note lacks the marker → the test goes red (verified).
- **Live** `crates/escurel-runner/tests/adk_live.rs`: the same flow with a
  **real adk-rust `LlmAgent`** runner (Gemini), `#[ignore]`'d (needs a built
  runner binary + `GEMINI_API_KEY`; non-deterministic/slow). Runnable with
  `cargo test -p escurel-runner --test adk_live -- --ignored`.

## How to recognise / build the live runner

Build a runner binary from the template speaking the `AdkHarness` contract
above (read `AdkTask` on stdin; read `ESCUREL_MCP_BEARER`; set
`Authorization: Bearer` on a streamable-HTTP `MCPToolset` → `mcp_endpoint`;
`.instruction(instructions)`; print a `HarnessOutcome` on stdout):

```text
git clone https://github.com/DataZooDE/datazoo-agent-template
cd datazoo-agent-template
cargo build --release --bin datazoo-agent-adk-runner
GEMINI_API_KEY=... LLM_PROVIDER=gemini \
  ESCUREL_RUNNER_ADK_BIN=$PWD/target/release/datazoo-agent-adk-runner \
  cargo test -p escurel-runner --test adk_live -- --ignored
```

If a future contributor sees "Python ADK / `MCPToolset` script" in the issue
and reaches for a Python runtime: it isn't here. The contract is the Rust
spawn-by-path adapter described above; the adk-rust tree stays external.
