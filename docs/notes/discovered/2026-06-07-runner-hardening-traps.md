# Agent-runner hardening traps (#158)

Three non-obvious things bit during the DLQ + quotas + observability +
graceful-shutdown work-item. Recognise them next time.

## 1. Throttling must reset the just-created ledger row, not leave it pending

`begin_run` creates a `pending` ledger row *before* the quota gate runs. If a
trigger is then throttled and we simply drop it, the `pending` row makes the
next poller cycle's `begin_run` return `InFlight` — so the event is silently
wedged and never re-driven. **Fix:** on a quota throttle, `mark(run_id,
Failed)`. `failed` is retriable (not idempotency-terminal, per #157), so
`begin_run` re-claims it next poll. Same applies to an `EnqueueOutcome::Full`
after admission. Symptom if you forget: over-quota events vanish instead of
being processed once the window rolls.

## 2. `unsafe_code = "forbid"` blocks an in-process `libc::kill`

The workspace lints `unsafe_code = "forbid"`, which `#[allow(unsafe_code)]`
*cannot* override (forbid is absolute). The SIGTERM-drain test therefore cannot
call `libc::kill` directly. **Fix:** shell out to the real `kill(1)` utility
(`Command::new("kill").arg("-TERM").arg(pid)`). It is still a genuine SIGTERM
to the child pid — a real signal, no mock.

## 3. Reading a cascaded event's trace_id only from `list_inbox` races the runner

A cascaded event transits the inbox briefly, then the runner processes it and
binds it to an instance — so it leaves `list_inbox`. A test that polls only the
inbox for `provenance.runner.trace_id` will usually see ≤ 1 hop and flake.
**Fix:** sweep BOTH the live inbox AND each instance's `list_events` history and
accumulate trace ids by event id across the polling loop. Processed events keep
their provenance in `list_events`.

## Design note: trace-per-lineage assertion without a collector

`escurel-obs` exposes an OTLP *export* pipeline but no in-memory `SpanExporter`
handle a test can read back. Rather than stand up a collector, the
`cascade_trace` DoD asserts the equivalent real signal the issue offers: the
`trace_id` the runner mints on the root run's span and carries forward in
`provenance.runner.trace_id` is **identical across every cascaded hop**, read
straight from the real gateway. Each hop's root span uses that id, so one trace
spans the lineage. This is fully real/no-mock and avoids a collector dependency.
