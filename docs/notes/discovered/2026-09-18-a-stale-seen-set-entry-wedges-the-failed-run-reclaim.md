# Three bugs behind one "parallelism flake": a wedged re-claim, a racing port, and a live API

**Date:** 2026-09-18 · **Found while:** chasing the `escurel-runner` suite
"parallelism flake" (two tests failing ~50% of full-suite runs).

## Symptom

A full `cargo test -p escurel-runner --test suite` failed on roughly half of
its runs, but never the same test twice in a row — variously
`harness_failure_vs_gateway::running_out_of_turns_with_nothing_drafted_is_still_a_failure`,
`workflow_end_to_end::verify_barrier_runs_against_gemini`, or
`gateway_not_ready::a_trigger_waits_for_a_gateway_that_is_still_booting`.
Each failing test waited out its **entire** deadline (180–240s) and reported
no progress at all:

```text
the run never reached a verdict; ledger says
{"total":1,"terminal":0,"succeeded":0,"failed":0,"dead_letter":0}
```

Every one of them passed in isolation, in under a second.

There turned out to be **three** independent causes wearing one costume. The
lesson is mostly about that: "flaky under parallelism" is a symptom, and
sampling one failure and generalising is how the first two wrong explanations
below got written down as fact.

## What it was NOT

Two plausible explanations were already written into the test file's comments,
and neither explained the hangs:

- **Load / oversubscription.** Measured during a failing run: load peaked at
  **6.46 on 32 cores** with 53 GB free. 67 of 68 tests finished in the first
  ~30 seconds; a single runner process then sat idle for the remaining ~200s.
  An idle machine, not a contended one.
- **`free_port()` TOCTOU** — as the cause of *the hangs*. `free_port` does
  bind-then-drop and is copy-pasted into 26 modules, so it looked like the
  obvious culprit. Forensics at the moment of a hang (`ss -ltnp` on the port,
  plus the child's pid) showed the port held by **this test's own runner**,
  alive, writing **its own** ledger file. No collision. (It is nonetheless a
  real bug with a different signature — cause 2 below. A hang and a fast
  failure are not the same flake.)
- **File-descriptor exhaustion** from per-call `reqwest::Client`s. Real, and
  worth the fix it got, but not this: the fd limit here is 524288.

## Cause

The runner keeps two things that both answer "have we run this event?": the
durable ledger, which is the stated authority, and an in-memory seen-set in
`DispatchQueue`, documented as "a cheap in-memory front" for it. They
disagreed, and the disagreement was unrecoverable.

`Ledger::begin_run` deliberately **re-claims** a `failed` row — resets it to
`pending`, mints a fresh run id, returns `Created` — so a transient failure is
re-drivable rather than wedged for ever (#157). But the seen-set is only ever
cleared by an operator DLQ requeue, **never on completion**. So every event
this process has already dispatched is still in it, and the re-claim ran
straight into a `Duplicate` from the cache:

```text
16:44:41.209  dispatch: permanent failure; recorded failed (retriable re-drive)
16:44:41.216  gate: run created + admitted; trigger enqueued  outcome=Duplicate
              (same event_id, fresh run_id)
              … then nothing, for 240s
```

`gate_and_enqueue` reset the row to retriable only for `EnqueueOutcome::Full`.
On `Duplicate` it left it exactly as the re-claim had: `pending`, with nothing
queued to move it. That state is a one-way door — the dispatch loop never sees
the run, and every later delivery reads `pending` and drops it as `InFlight`.
The `failed` verdict an operator would have read was gone too, cleared by the
re-claim that then failed to dispatch.

So the #157 re-drive path could not work **at all** within a process that had
already dispatched the event, and each attempt cost a verdict.

## Fix (cause 1)

Two changes in `gate_and_enqueue`, both narrow:

1. Drop this event's seen-set entry before enqueueing in the `Created` arm.
   Reaching `Created` is proof no run for the event is in flight — `begin_run`
   is one `IMMEDIATE` transaction with `ON CONFLICT DO NOTHING`, so a
   concurrent delivery gets `InFlight`/`AlreadyTerminal` and exactly one
   caller is handed `Created` — therefore an entry still present is stale by
   construction and safe to forget.
2. Reset the row to retriable `failed` on **every** non-`Enqueued` outcome,
   not just `Full`. The row exists before the enqueue, so any outcome left
   unreset is a wedged run.

Adding a `LedgerDecision::Reclaimed` variant was considered and rejected:
`Created(_)` is asserted in ~25 ledger tests, and the gate does not actually
need to tell the two apart once the invariant above is stated. Clearing the
seen-set on run completion instead was also rejected — it reopens a
double-dispatch window that the set exists to close.

## Why the tests were flaky rather than red

The control test asserts a `failed` verdict, and the runner does record one.
Whether the test saw it was a race between its own 250ms ledger poll and the
poller's 250ms inbox re-poll, which erased it ~7ms after it was written. The
test was a real detector of a real bug, sampling at the wrong moment; the
generous 240s deadline added earlier made it look like a slow-CI problem.

`a_failed_run_redelivered_dispatches_instead_of_wedging_pending` now pins it
deterministically at the gate, in 0ms, with no processes involved. Six
consecutive full-suite runs after the fix: 68 passed, and no run exceeded 52s
(the hang signature was 180–240s).

## Cause 2: `free_port()` really is racy — it just fails differently

With cause 1 fixed the suite went 6/6 green, then failed again — this time in
**11 seconds**, not 240, and with a line nobody had captured before:

```text
Error: Address already in use (os error 98)
```

`fn free_port()` (26 copies, one per module) binds `127.0.0.1:0`, reads the
port, drops the listener, and hands the number to a child. Two concurrent
callers get the **same** port once both probe listeners are dropped; one
runner then dies on bind, and the test waiting on *that* runner fails at its
own deadline naming something unrelated — here
`curate_generates_a_derivable_by_category_index` "no index instance … within
45s". Nothing in that message points at a port.

Fixed by `escurel_test_support::free_port`, which remembers every port it has
handed out and never repeats one. The whole `escurel-runner` suite is a single
test binary (deliberately, for link time), so within a suite this is exact
rather than probabilistic. The 26 local copies are gone.

It does not close the window against an *unrelated* process taking the port
between probe and child bind. The real end state is for the child to bind `:0`
itself and report back — `escurel-runner` already logs its actual
`local_addr` — which is worth doing if this ever resurfaces.

Effect: 8/8 CI-like runs green, and the suite's wall clock collapsed from a
ragged 11–47s to a flat 15.2s. The variance *was* the bug.

## Cause 3: a live-API test that only runs on this machine

`workflow_end_to_end::verify_barrier_runs_against_gemini` self-skips unless
`GEMINI_API_KEY` is set. It is set in this developer's shell, so locally the
test calls the **real** Gemini API; CI has no key, so it skips. That is why CI
was green throughout while local `--workspace` runs flaked.

Its signature is bimodal and unlike the other two: at the same machine load it
either finished in ~37s or made **zero** progress for its full 180s. That is
upstream latency, not an escurel defect, and it is deliberately left alone —
papering over it with a longer deadline is what hid cause 1 for so long. Worth
knowing before blaming the runner: `env -u GEMINI_API_KEY cargo test` is the
CI-equivalent run.

## Lesson

A cache in front of an authority needs a stated invalidation rule, or the
authority's decisions become advisory. That was cause 1, and it was a product
bug, not a test problem.

The method mattered more than any single fix. "Flaky under parallelism" was
three unrelated faults, and every cheap explanation on offer was about the
*environment* — load, file descriptors, ports — with two of them already
written down as fact in the test's own comments. What separated them was
measuring instead of arguing:

- **A full-deadline timeout with zero progress is evidence against a
  contention story, not for it.** Load peaked at 6.46/32 while one runner sat
  idle for 200s. One command.
- **Distinguish failures by their shape before grouping them.** 240s-hang,
  11s-bind-error and 37s-or-stall were three bugs; treating them as one flake
  is what kept them alive.
- **Capture the subject's own logs.** The runner's log said `recorded failed`
  seven milliseconds before the poller re-delivered the event as
  `Duplicate` — the whole diagnosis, in two adjacent lines, invisible until
  the child's stdout was redirected to a file.
