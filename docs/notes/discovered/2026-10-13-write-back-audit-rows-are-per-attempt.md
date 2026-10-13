# Write-back audit rows are per attempt (first-writer-wins)

**Symptom.** A non-idempotent write-back (no `idempotent: true`, not PUT) could be sent twice.
Sequence: promote 1 hits an outage before anything is sent and records `…:failed` with
`attempts: 0`; promote 2 reaches the upstream, the change applies, the answer is lost, and its
own `failed` record is silently dropped (`capture_event` is `ON CONFLICT DO NOTHING`); promote 3
reads the stale `attempts: 0` row via `definitely_not_applied`, believes nothing was ever sent
and sends again.

**Fix.** The remote path writes `write-back:<draft>:<n>:applying|failed` per attempt; the
"is an earlier attempt unresolved?" check reads only the previous attempt's rows.
`write-back:<draft>:applied` stays unnumbered (a final witness). The SQL path keeps the old ids:
its retries are guarded by the row's etag, not by these rows.

**Recognise it.** A doc comment saying an audit event "rewrites the same row" is wrong for
`capture_event`; any logic reading an audit row for "latest state" needs a distinct id per state.

## Changelog lines for the release notes
- Write-back audit events for REST upstreams are now per attempt (`write-back:<draft>:<n>:applying|failed`);
  a non-idempotent write that may have applied is no longer re-sent after an earlier outage.
