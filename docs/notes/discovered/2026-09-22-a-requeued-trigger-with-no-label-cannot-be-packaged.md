# A requeued trigger with no label cannot be packaged

**Symptom.** After a DLQ requeue (or, since the workbench backend P2-3b, a
`retry` control) the ledger showed TWO fresh runs for the event: one that
went straight to `failed` with `dispatch: packaging failed … resolve:
markdown parse error: missing frontmatter`, then one the poller re-claimed
that actually ran. The run id the requeue reported was the failed one.

**Cause.** The requeue path enqueued a bare `Trigger` — `label_skill: ""`,
no instance, root lineage — straight onto the dispatch queue. Packaging
resolves the skill by the trigger's label, so `resolve("[[]]")` failed,
the attempt was recorded as a permanent failure, and the row went
`failed`; the poller then re-claimed the still-inbox event under a third
run id and ran it. Everything converged, which is why nobody noticed: the
DLQ test only asserted the terminal block was cleared.

**Fix.** `enqueue_requeued` re-reads the event by id (through the same
lineage trust gate the poller applies) and builds the trigger with
`Trigger::from_event_gated`, dropping only the content hash (a requeue is
an explicit re-run; the dedup must not refuse it). The bare trigger is the
fallback for a runner with no gateway credentials.

**How to recognise it.** A requeue followed immediately by
`dispatch: packaging failed` with a `resolve` error, and a run id that
differs from the one the requeue answered with.
