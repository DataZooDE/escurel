# Load flakes: six tests that fail only on a starved machine, and how to measure them

**Symptom.** The pre-push hook (`scripts/check.sh`, the full workspace test run) failed on a different test each
time and passed on a plain retry: `promotion_cascades` (runner), `runner_status` (runner),
`rows_paging_keys` (server, "a source query that runs too long is interrupted"), `binary_boots`
(`rebuild_index_on_boot_always`), `connect_retry` (client: `the call succeeds: Http { status: 404 }`), and, in
the VS Code component suite, 17-27 thread-canvas tests timing out. Every failure was seen while this machine's
load average was 55-70 on 32 cores (several forks building and testing at once); none at load below ~15.

**Measured.** `scripts/flaky-hunt.sh 10` (ten runs of each file's tests, one `cargo test` per run, binaries built
once) on 2026-10-10 at load 7-16: `promotion_cascades`, `runner_status`, `agent_token_narrowing`,
`rows_paging_keys`, `binary_boots`, `connect_retry`: 10/10 passed each. So there is no defect that fails on an
idle machine; these tests have timing windows (heartbeat intervals, "interrupted within N ms", a boot deadline,
polling loops) that CPU starvation stretches past their limit.

**One suspected cause worth knowing.** `connect_retry`'s 404 is not a timing window: the test asks the OS for a
free port, closes it, and the retry then connects to that port. When something else (a docker-published port,
another test binary) takes the port in between, the retry reaches a stranger and gets its 404. The same
probe-then-bind race was fixed for the Postgres container helper (`Pg::start` now retries when docker refuses
the probed port). If `connect_retry` ever fails off a loaded machine, look there first. Not changed here: it was
not reproduced at 10/10.

**How to recognise it next time.** A hook failure that names one test, passes alone, and happened while
`cat /proc/loadavg` shows more than ~2 per core. Re-run once. If it fails twice, it is not this.

**What to do about it.**
- Measure, do not guess: `scripts/flaky-hunt.sh [runs] [file ...]` prints pass/fail counts per test file and
  keeps the failing runs' output; run it while the machine is busy (that is the point). `FLAKY_REPO=<checkout>`
  uses a checkout whose `target/` is already built.
- Component tests on a loaded machine: `ESCUREL_WTR_TIMEOUT_MS` / `ESCUREL_WTR_FINISH_MS` (see
  `editors/vscode/demo/README.md`); e2e: `ESCUREL_E2E_SLOW=4`.
- Do not widen a timeout in the test to make the number go away unless the hunt shows it failing on an idle
  machine.
