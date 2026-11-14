# `wait_for_status` must name the ledger's exact status strings

**Symptom.** `run_transcript::a_codex_shell_command_is_a_redacted_command_record` failed in CI now and then
with `run never reached ["pending", "processed", "failed", "dead_lettered"]`. The runner's log showed the run
dead-lettered about 100 ms after it was admitted. It passed locally and on most CI runs.

**Cause.** The ledger writes `dead_letter` (see `escurel-runner-core/src/ledger.rs`), not `dead_lettered`. The
dispatch log says "dead-lettered", which is where the wrong spelling came from. Normally the test's first poll
sees `pending` or `failed` first and returns. On a loaded runner the run reached its terminal status before
that first poll, and no listed status matched.

**How to recognise it next time.** A `wait_for_status` timeout where the runner's log shows the run reached a
terminal state long before the deadline. Copy status strings from `ledger.rs`, not from log messages.
