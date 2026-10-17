# Load flakes: six tests that fail only on a starved machine, and how to measure them

**Symptom.** The pre-push hook (`scripts/check.sh`, the full workspace test run) failed on a different test each
time and passed on a plain retry: `promotion_cascades` (runner), `runner_status` (runner),
`rows_paging_keys` (server, "a source query that runs too long is interrupted"), `binary_boots`
(`rebuild_index_on_boot_always`), `connect_retry` (client: `the call succeeds: Http { status: 404 }`), and, in
the VS Code component suite, 17-27 thread-canvas tests timing out. Every failure was seen while this machine's
load average was 55-70 on 32 cores (several forks building and testing at once); none at load below ~15.

**Measured.** `scripts/flaky-hunt.sh 10` (ten runs of each file's tests, one `cargo test` per run, binaries built
once) on 2026-10-10 at load 7-16: `promotion_cascades`, `runner_status`, `agent_token_narrowing`,
`gateway_not_ready`, `rows_paging_keys`, `binary_boots`, `connect_retry`: 10/10 passed each. So there is no defect that fails on an
idle machine; these tests have timing windows (heartbeat intervals, "interrupted within N ms", a boot deadline,
polling loops) that CPU starvation stretches past their limit.

**The one that is not a timing window: a stranger answers on the probed port.** Two failures carry the same
signature: `connect_retry` (`the call succeeds: Http { status: 404 }`, seen in the hook) and
`gateway_not_ready::a_trigger_waits_for_a_gateway_that_is_still_booting` (`the trigger is accepted: left: 404,
right: 202`, `gateway_not_ready.rs:99`, seen on a 2-core CI runner, PR #678, a docs-only change). In both the test
asks the OS for a free port (`free_port()`: bind port 0, read it, close), starts something on it, and waits for
`/healthz`; a 404 on the NEXT request means a different server is answering. Test binaries here run their tests in
parallel and many of them bind port 0 for a mock upstream, and the kernel hands a just-freed port out again
immediately, so a parallel test's mock can take the probed port before the runner (or the retried call) reaches
it. If the runner then fails to bind, it exits, and the stranger's 200 on `/healthz` (or its 404 on `/trigger`)
is what the test sees. The same probe-then-bind race was fixed for the Postgres container helper (`Pg::start`
retries when docker refuses the probed port). It is not reproduced here (10/10 at low load, including this test),
so no code was changed; the fix to make when it recurs is in the test helper: after spawning, check the child is
still alive (`try_wait`) and respawn on a new port if it exited, instead of trusting the first `/healthz` 200.

**How to recognise it next time.** A hook failure that names one test, passes alone, and happened while
`cat /proc/loadavg` shows more than ~2 per core. Re-run once. If it fails twice, it is not this.

**What to do about it.**
- Measure, do not guess: `scripts/flaky-hunt.sh [runs] [file ...]` prints pass/fail counts per test file and
  keeps the failing runs' output; run it while the machine is busy (that is the point). `FLAKY_REPO=<checkout>`
  uses a checkout whose `target/` is already built.
- Component tests on a loaded machine: `ESCUREL_WTR_TIMEOUT_MS` / `ESCUREL_WTR_FINISH_MS` (see
  `editors/vscode/README.md`); e2e: `ESCUREL_E2E_SLOW=4`.
- Do not widen a timeout in the test to make the number go away unless the hunt shows it failing on an idle
  machine.

## Update 2026-10-17: three more, one with the stranger's fingerprint, and the fix for the pattern

`remote_rest_rows::an_unreachable_source_is_counted` (CI, #731 attempt 1, `cargo test` 308 s on a 2-core runner),
`sigterm_drain::sigterm_drains_and_restart_is_consistent` and
`workflow_end_to_end::distill_stamps_last_verified_on_the_woven_page` (both the pre-push hook on #733, passed on
rerun).

- **`an_unreachable_source_is_counted` is the stranger-on-the-port race, with proof.** The failed run's `/metrics`
  shows `escurel_egress_total{outcome="ok"} 1` next to `{outcome="error"} 2` and **no**
  `escurel_source_unavailable_total` line: one outbound call to the "dead" source succeeded. `dead_base()` bound
  port 0, read the port, dropped the listener and handed the number to a gateway that connected to it ~1.9 s
  later (that `expand` took 1937 ms on the starved runner); in that window a parallel test's mock took the port.
  Fix: `dead_base()` now returns a guard that keeps the port **bound but not listening** for the test's life, so a
  connect is refused (`ECONNREFUSED`) and nothing else can be handed the number
  (`a_dead_base_stays_dead_and_unclaimable_while_held` pins both). Two more probe-then-drop sites remain
  (`remote_backend_tools.rs:732`, `gateway_lost_mid_life.rs:59`); change them the same way if they ever flake.
- **`sigterm_drain` and `distill_stamps_last_verified` did not reproduce** (25/25 each at load average ~50 on 32
  cores with 48 CPU hogs added; the tests take 1-5 s against 15-45 s deadlines), and the original failure output
  was not kept, so their cause is not established. They share the pattern named above (a runner on a `free_port()`
  that can die at bind while a stranger answers, or just start late under load). What changed: both now fail by
  name when the runner has already exited (`runner exited before it became healthy at <addr>: <status>`), and
  `spawn_echo_runner` now returns only after `/healthz` answers, so the 45 s workflow deadlines measure the
  workflow, not process start. If either flakes again the message says which of the two it was.
- `scripts/flaky-hunt.sh` knows the three files now.
