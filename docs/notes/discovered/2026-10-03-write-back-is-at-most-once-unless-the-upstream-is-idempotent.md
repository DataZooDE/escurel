# Write-back: at-most-once unless the upstream can dedupe

**Symptom to avoid.** A promoted change is sent upstream, the response is lost (timeout, reset), and a
retry applies it twice, or a restart re-sends it.

**Design** (`crates/escurel-server/src/write_back.rs`). Promote runs a hook, after the human gate:
audit first (`escurel:write-back` system event `write-back:<draft>:applying`), re-read the row and compare
its etag (`w1:<sha256 of the projected columns>`; moved means `write_back_conflict`), then send. REST gets
`Idempotency-Key` (the draft id) and `If-Match`; MCP gets the declared `idempotency_arg`. Transient
failures (5xx, transport, 429) are retried a few times with jittered backoff; a 4xx is final
(`write_back_rejected`). The `applied` event is the witness that it happened.

A REST write op counts as repeatable only when the skill declares `write: {…, idempotent: true}` (or the method is
`PUT`); before 2026-10-10 every HTTP write was assumed idempotent, so an upstream that ignored `Idempotency-Key`
could apply a change up to three times after a lost answer.

An endpoint WITHOUT idempotency support is attempted once, and if an `applying` event exists with no
outcome the write is refused as `write_back_unknown_outcome` rather than repeated: a human reconciles.

**Gotchas.** `applying` and the outcome are written in the same instant and can carry the same timestamp,
so "the latest write-back" must break ties toward the outcome (`latestWriteBack` in the extension showed
"being sent" for an applied change before that). The draft stays open after a failure, so promoting again
is the retry. Audit events carry column names and a patch hash, never the values.
