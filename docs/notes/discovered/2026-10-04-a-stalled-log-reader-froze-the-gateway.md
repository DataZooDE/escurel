# A log reader that stopped reading froze the whole gateway

**Symptom.** A consumer validated its pages one by one against `escurel-test-gateway` (the same
engine as `escurel-server`). Every run stopped answering at the SAME call, about the 88th, and never
recovered: `/healthz` unreachable, 94 of 96 threads in futex wait, one tokio worker in
`anon_pipe_write`. Skipping a page moved the hang to the 89th call: it counted requests, it did not
depend on the page. Variation between runs (10, 11, 87, 88) was only how much each run had logged.

**Cause.** The gateway logs one JSON line per request (~730 bytes) to stdout (the substrate log
contract) with `tracing_subscriber`'s `with_writer(io::stdout)`: a synchronous write from the logging
thread while holding the stdout lock. The consumer read the connection line and then left the pipe
alone, so the 64 KiB pipe filled after ~90 requests, the write blocked, and every other thread that
logged queued behind the lock. Not a regression of the OKF branch: `origin/main` has the same code
since the observability crate landed (May 2026). Any stuck log shipper, a backed-up `docker logs`, or
a harness that only wants the first line reproduces it.

**How to recognise it.** `ls -l /proc/<pid>/fd/1` is a pipe, `/proc/<pid>/task/*/wchan` shows one
`anon_pipe_write` and the rest `futex_do_wait`; `strace`/`gdb` may be blocked by ptrace policy, the
wchan listing is enough. Redirecting stdout to a file makes the hang disappear.

**Fix.** `escurel-obs` logs through a bounded queue to ONE writer thread (the only place a write may
block). A full queue DROPS the line and counts it (`escurel_log_lines_dropped_total`); once the reader
is back the writer emits a `logs.dropped` line saying how many were lost; the telemetry guard flushes
on drop. Regression test: `escurel-test-support/tests/test_gateway_bin.rs`
`a_log_reader_that_stops_reading_does_not_stall_the_gateway` (real binary, a held-open undrained pipe,
400 calls; red at call 88 before the fix).

**For harness authors.** A child's stdout is a pipe someone has to drain: read it on a thread, or
redirect it to a file. The gateway no longer freezes when you do not, but it will drop log lines.
