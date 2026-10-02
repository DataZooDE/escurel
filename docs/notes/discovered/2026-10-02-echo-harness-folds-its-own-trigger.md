# The echo harness folded the oldest inbox event, not the one its run was started for

**Symptom.** Test and demo flakes that looked unrelated: a run started for a new event did nothing
visible, or dead-lettered, while an OLDER event was still waiting on a human. A `review`-autonomy run
leaves its event in the inbox until its draft is promoted, so "an older event is waiting" is the
normal state, not an edge case. The VS Code suites grew `settleInbox` / `discardOpenDrafts` helpers
to work around it.

**Cause.** `escurel-runner`'s echo harness (`src/bin/echo_harness.rs`) folded the OLDEST inbox event
that had a target page. The run's own event was ignored, so the new run folded the old event, tried to
draft on a page that already held an open draft, and dead-lettered (or drafted for the wrong event).

**Fix.** The harness reads the trigger from the run's task input (`event_id: <id>`) and folds exactly
that event; with no trigger line it falls back to the newest targeted event. Pinned by
`crates/escurel-runner/tests/suite/echo_trigger.rs` (real gateway, real runner, real echo harness; red
before the change).

**Recognise it next time.** Echo-harness runs that act on the wrong page, or a dead letter whose
draft belongs to a different event than the run's trigger. Test-only code: the production harnesses
get their trigger the same way, from the task input.
