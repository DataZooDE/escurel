# 2026-10-12 — closing a thread is a durable gateway event (BACKEND_GAPS PR-9)

## Changelog lines for the release notes

Suggested entries for the shared changelogs (skill `VERSION` /
`.claude/skills/escurel-platform/CHANGELOG.md`, root `CHANGELOG.md`,
`docs/notes/breaking-wire-changes.md` — not edited here: three parallel
tasks share them):

- Gateway: new `capture_event` carve-out `escurel:thread-closed` with body
  `{action: close|reopen, root_event_id, reason?}`. Whoever may read the
  root event may close or reopen its thread; run-bound machine tokens are
  refused; unknown or unreadable roots answer `event_not_found` (no
  existence oracle); re-asking the current state is a no-op (`already:
  true`). Stored `kind: system` on the root's page with a server-stamped
  `provenance.thread` block (additive `root_title` included); read closed
  threads via `list_events { label_skill: "escurel:thread-closed" }` — no
  new tool. Non-breaking: additive label + one additive response flag.
- VS Code: the Threads outline ends in a collapsed **Closed** group (title,
  closer, time; opens the thread on click), with `escurel.closeThread`
  (asks first, reason optional) and `escurel.reopenThread` in the palette
  and the outline menu. Closing deletes nothing and leaves Overview,
  Awaiting you and Inbox alone.
- Consumer skill: `02-tool-surface.md` now lists three `escurel:`
  carve-outs; `11-event-driven-agents.md` gained a `Closing a thread`
  section.

## Notes for next time

- Symptom during implementation: the new close row did not appear under
  `list_events { root_event_id }` in the test. Cause was the test, not the
  server: the `root_event_id` branch hides `kind: system` rows unless
  `include_system: true` is passed (only the label-alone branch implies it
  for `escurel:` labels). The thread's own record read needs the explicit
  flag; the outline's label listing does not.
- `event_to_json` + an early return inside `tool_capture_event` is the
  established way to answer an idempotent re-request without filing
  (`replayed` does the same for `event_id` re-capture); the `already: true`
  flag follows that shape.
- The extension's Threads outline holds exactly one open thread, so the
  "Closed" group is a second top-level row in `ThreadsTree`, not part of
  `outlineRows` — `closedGroupRow` builds it and `setClosedThreads` feeds
  it, keeping the single-thread outline untouched.

## Review hardening (Claude, before landing)

- `provenance`: only the server's `thread` block is stored; a caller's `runner` / `review` / `control` keys used to survive next to it (red test `a_close_request_keeps_nothing_of_the_callers_provenance_but_the_thread_block`).
- `reason` is capped at 200 characters (refused with `invalid_params`); `root_title` is stamped cut to 200.
- The extension cleans title, closer and reason (bidi/control characters, 400-char cap) where the fold builds its model.
- Pinned: the carve-out does not widen the reserved `escurel:` namespace for a human; concurrent closes leave rows the fold reads.
