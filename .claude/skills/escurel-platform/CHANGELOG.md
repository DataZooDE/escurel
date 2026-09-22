# Changelog — escurel-platform skill

The skill version tracks the consumer-facing contract, not the Escurel
binary version. The Escurel repo's checked-out git ref is the true version
pin (see `SKILL.md` → "How this skill is installed").

## 0.6.58 — the runner honours the skill contract (workbench P2-7b)

- `autonomy: confirm` on a skill holds the write exactly like `review`
  (a draft, a changeset, no cascade) and the run's events say
  `autonomy: confirm`, so the workbench knows to ask the human explicitly.
- A skill page's `harness:` is honoured like a manual start's, within
  `ESCUREL_RUNNER_HARNESS_ALLOW` (a workflow step's own declaration still
  wins; a name outside the list fails the run closed).
- A skill's `actions:` limits which skills its confirmed writes cascade
  to (undeclared = any cross-skill write, as before); `cascade.max_depth`
  caps how deep its chains go (the runner's global
  `ESCUREL_RUNNER_MAX_DEPTH` still applies); `cascade.target` pre-flags
  the hop (the flat `cascade_target:` still works).

## 0.6.57 — the skill contract keys: `summary`, `harness`, `actions[]`, `cascade{}` (workbench P2-7a)

- `list_skills` rows carry what a skill page declares: `summary` (the
  one-liner for skill lists), `harness` (the adapter it asks for),
  `actions` (the skills it may fan out to), `cascade {target, max_depth}`.
  Absent keys stay absent.
- `validate` lints them on a skill page: `summary_missing` (warning),
  `summary_too_long` (> 200 characters), `harness_unknown` (not one of
  echo | claude | codex | agy | muse | gemini | delegate),
  `action_skill_unknown` (an entry naming a skill the corpus does not have,
  at `frontmatter.actions[i]`; a non-list at `frontmatter.actions`).
  The runner's side (honouring `harness`, capping `cascade`, filtering by
  `actions`, `autonomy: confirm`) is P2-7b.

## 0.6.56 — `mint_agent_token`: the gateway mints run-bound agent bearers (workbench P2-6)

- New tool `mint_agent_token { skill, root_event_id?, target_page_id?,
  ttl_secs?, trace_id? }` → `{token, run_id, root_event_id, subject,
  expires_at}` (CLI `run mint-token --skill …`). The bearer is
  `agent:<skill>` acting for you (`act.sub` = your subject), carries YOUR
  authority and never more (an admin's mint is admin; a member's mint is
  their groups), `purpose: workbench_agent`, and a fresh `run_id` — so
  drafts it makes are stamped and `report_progress` accepts it. The
  gateway writes `run-started` (harness `workbench`) at mint and closes
  the run `run-finished { status: "expired" }` if the token lapses
  without a terminal (swept every 30 s, in memory). Refused `unsupported`
  on a gateway without `ESCUREL_AUTH_SIGNING_KEY` (+ `_KID`, `_ISSUER`).
- The signing identity (`Signer`) now lives in `escurel-auth`; the runner
  re-exports it unchanged.

## 0.6.55 — plan mode and approval (workbench P2-5b)

- A manual start with `mode: plan` runs the harness on a no-write tool
  surface (the review surface minus every write) with a "plan only"
  instruction: report the whole plan once via `report_progress` (every
  step `pending`) and stop. The run ends `planned` — a new terminal:
  nothing landed, the event stays in the inbox, the poller never re-runs
  it — and `run-finished { status: "planned", plan }` carries the plan.
  claude plans natively (`--permission-mode plan`), gemini and echo on the
  narrowed surface; codex / agy / muse / delegate refuse (`failed`).
- A manual start with `approved_plan_run_id` gets that run's plan
  injected at the top of its input ("## Approved plan (run …)") and runs
  for real. `/debug/ledger` and the runner status gain a `planned` count.

## 0.6.54 — manual start: `provenance.manual {harness?, mode?}` (workbench P2-5a)

- A human starts a run by hand by capturing an ordinary event with a
  `provenance.manual` block. The gateway stamps `requested_by` from the
  token (a caller's value is replaced), defaults `mode` to `run` and
  refuses anything but `run` / `plan`. The runner honours `harness` only
  within `ESCUREL_RUNNER_HARNESS_ALLOW` (default: its configured harness)
  and otherwise fails the run closed — `run-finished.reason` says which
  harness and why — rather than running the default in its place. The
  run's `run-started` carries `provenance.runner.manual`. `run-finished`
  gains `error` — the last attempt's own message — beside the `reason`
  slug for a failed run. Plan mode itself lands next
  (P2-5b).

## 0.6.53 — runner status heartbeat; `list_events { newest_first }` (workbench P2-4)

- The runner reports its health as unassigned `escurel:runner-status`
  system events: `started` on boot, `changed` at once when what it reports
  changes (a run starts or ends, a tenant pauses), `heartbeat` every
  `ESCUREL_RUNNER_STATUS_INTERVAL` (default `30s`) otherwise, `stopping` on
  drain. Body: `runner_id` (`ESCUREL_RUNNER_ID`), `version`, `harness`,
  `tenant`, `live_runs[{run_id, event_id, instance_page_id}]`,
  `paused_tenants`, `runs{pending, processed, failed, dead_letter,
  cancelled, total}`, `throttled{…}`, `harness_permits_available`,
  `uptime_s`, `last_poll_age_ms`. The gateway keeps a tenant's last 50 rows.
- `list_events` gains `newest_first` (CLI `event list --newest-first`):
  turn any listing around; with `limit: 1` it is the latest row — how the
  runner's status is read.

## 0.6.52 — the runner acts on `escurel:run-control` (workbench P2-3b)

- The runner tails `escurel:run-control`, acts on each request — `cancel`,
  `retry`, `pause`, `resume`, `requeue` — and answers it once under
  `escurel:run-control-result` (a system event in the run's own record,
  on the page the request was filed on; `body.outcome` is `cancelled` |
  `not_live` | `requeued` (+ `new_run_id`) | `paused` | `resumed` |
  `refused` (+ `detail`)). `pause` holds a tenant at the gate: nothing is
  claimed, events stay in the inbox until `resume`. `retry` re-drives a
  `failed` / `cancelled` / `dead_letter` run under a fresh run id.
- A requeue (DLQ or `retry`) now rebuilds its trigger from the real event;
  it used to enqueue a bare trigger that failed packaging and left one
  extra `failed` run per requeue.

## 0.6.51 — a live run can be cancelled (workbench P2-3a)

- The runner stops a live run on request: the harness subprocess gets
  SIGTERM, `ESCUREL_RUNNER_CANCEL_GRACE` (default `5s`), then SIGKILL; the
  Gemini loop stops between turns. The ledger records the run `cancelled`
  (terminal: the poller never re-runs it; a `retry` control will), its
  `run-finished` says `status: cancelled` with the requester's reason,
  nothing lands, the trigger stays `inbox`, no cascade. Today the seam is
  the runner's `POST /debug/cancel {run_id | tenant+event_id, reason?}`;
  the `escurel:run-control` subscriber (P2-3b) goes through the same path.
  `/debug/ledger` gains a `cancelled` count.

## 0.6.50 — controls as events: `escurel:run-control` (workbench P2-2)

- `capture_event` accepts the `escurel:run-control` label from non-admins:
  `cancel` / `retry` name a `run_id` and need write access to the run's
  target page (the same `ESCUREL_WRITE_ACL` gate as `update_page`);
  `pause` / `resume` / `requeue` are admin-only. A denial and an unknown
  run both read `event_not_found`. The request is stored as bookkeeping
  (`kind: system`) on the run's target page with
  `provenance.control.requested_by` stamped from the token. The runner's
  subscriber that acts on these lands next (P2-3b).

## 0.6.49 — a promoted draft cascades (workbench P2-1)

- The runner tails `escurel:review` and, on `draft-promoted`, emits the
  cascade the drafting run could not: from the promoted page, under that
  run's lineage (`parent_run_id` = the run), one cascade per draft however
  often the decision is announced. A review-gated chain now continues past
  the human — the second half of "cascade fires on promotion".

## 0.6.48 — `list_events` tails a label; review events name the trigger (workbench P2-0)

- `list_events { label_skill }` alone lists every event under that label,
  any status (an `escurel:` label implies `include_system`); with another
  selector it narrows. Every `list_inbox` / `list_events` page now carries
  `resume_cursor` — the cursor of its LAST row, full or not — so a poller
  tails a label: pass it back as `cursor` and the next page is exactly what
  arrived since. `next_cursor` keeps its meaning (more rows already exist).
- `provenance.review.event_id` (and `body.event_id`) = the draft's trigger
  event on every review event.
- CLI: `event list --label <skill>`.

## 0.6.47 — lineage resume by position; malformed re-subscribe clears the old one

- A lineage-scoped `since_event_id` resumes by log POSITION (everything
  after the row you named; everything if unknown) instead of comparing
  ids — run events carry deterministic non-ULID ids, so the compare could
  skip a run's terminal.
- A malformed `event_subscribe` replacing a live subscription now drops the
  old one too ("subscribes nothing" means nothing).

## 0.6.46 — filtered `event_subscribe` + gap-free lineage resume (workbench backend P1)

- `event_subscribe` takes `filters: {root_event_id?, run_id?, label_skill?,
  kind?, instance_page_id?}`; the predicate runs server-side before the ACL,
  so a workbench watching one thread subscribes to that thread. A malformed
  filter is refused (`invalid_subscription`) and subscribes nothing.
- With a `root_event_id` / `run_id` filter, `since_event_id` replays from the
  lineage's event log (any status, system rows included) — gap-free for
  the thread. Unfiltered resume stays inbox-only, as before.
- The gateway's push buffer grew from 256 to 1024 events per subscriber
  before `event_lagged`.

## 0.6.45 — `list_lineage` (workbench backend P1)

- New agent tool `list_lineage {root_event_id, include?, limit?, cursor?}` →
  `{root_event_id, nodes:[{id, type, parent, state, …}], next_cursor?}`:
  everything under a root event — the root and its cascade events, the runs
  folded from their `escurel:run` rows (state, harness, attempts, summary,
  newest plan), the changesets and drafts those runs proposed. Parents
  alternate event → run → {changeset → draft | draft | event}; the client
  folds the tree. A node the caller may not read is absent with its subtree.
  CLI: `escurel run lineage --root-event <id> [--include events|runs|drafts]`.

## 0.6.44 — review transitions on the bus (workbench backend P1)

- Every draft / changeset transition is published as an `escurel:review`
  `kind: system` event on the draft's target page: `draft-created`,
  `draft-promoted`, `draft-discarded` (incl. a stale draft superseded by a
  re-draft), `changeset-promoted`, `changeset-discarded`,
  `changeset-already_decided`. `provenance.review` carries `{draft_id,
  changeset_id, run_id, root_event_id, decided_by, already_decided}` — the
  lineage from the draft ROW, never the caller. Hidden from `list_inbox` /
  `list_events` unless `include_system`; pushed to `event_subscribe`
  subscribers like any event. A review queue no longer needs to poll.

## 0.6.43 — `report_progress` (workbench backend P1)

- New agent tool `report_progress {plan:[{step,status}], current?, note?}` →
  `{ok, event_id, run_id, steps}`: a harness reports its WHOLE plan as a
  snapshot, filed as a `run-progress` system event under `escurel:run` for
  the run the caller's token names. Only a run-bound bearer may call it (an
  admin token without a run is refused `-32602`); idempotent per snapshot;
  a run keeps its last 50. CLI: `escurel run progress --step a=completed …`.

## 0.6.42 — drafts record the run that proposed them (workbench backend P1)

- `Draft`, `Changeset` and `diff_draft` carry `run_id` / `root_event_id`:
  the run a held write came from and that run's lineage root. Stamped by
  the server from the caller's per-run agent token (`references/08`) —
  there is no argument for them, and a caller-supplied one is ignored. A
  human's draft carries `null` for both.

## 0.6.41 — event `kind` + lineage columns (workbench backend P1)

- Every event now carries `kind` (`user` | `system`), `root_event_id` and
  `run_id` on the wire. `system` = bookkeeping ABOUT a run (`escurel:run`,
  `escurel:review`, …), written by the runner and the gateway — admin-only
  to capture. It skips the inbox: captured with `instance_page_id` it is
  stored `processed` on that page at once, no `assign_event`.
- `list_inbox` and `list_events` HIDE `kind: system` rows unless
  `include_system: true`. Nothing changes for a consumer that never asks.
- `list_events` gains two selectors beside `instance_page_id` / `event_id`:
  `root_event_id` (a lineage — the root and everything captured under it,
  any status, oldest first) and `run_id` (one run's own events; implies
  `include_system`). Exactly one listing selector, or `-32602`.
- `root_event_id` / `run_id` are promoted from `provenance.runner` at
  capture (server-side; there is no argument for them). A user event that
  names no root is its own root. A cascade hop names the run that emitted
  it as `provenance.runner.parent_run_id`, not as its own `run_id`.
- CLI: `event capture --kind`, `event inbox --include-system`,
  `event list --instance | --root-event | --run [--kind] [--include-system]`.

## 0.6.40 — branches: registry, write context, tombstones, merge

- `create_branch` / `list_branches` / `merge_branch` / `abandon_branch`, plus
  `escurel branch create|list|merge|abandon` and typed client methods. A
  branch is an isolated workspace with an author, the corpus state it forked
  from, and a status that moves `open → merged | abandoned` exactly once.
- **`update_page` and `delete_page` take `branch`.** This is the important
  part: the branch is a property of the WRITE, not of the page. The server
  derives the overlay page id and stamps `scenario`, so an agent working on a
  branch cannot forget to stamp a page and write to production instead — the
  failure mode author-supplied `scenario:` frontmatter has. That frontmatter
  still works and is not broken; it is simply no longer the only way.
- **`delete_page` with `branch` is a TOMBSTONE**, not a retraction: the base
  page is untouched, the slug reads as absent on that branch, and the delete
  lands for real when the branch merges. An overlay that could only add or
  override could not express "this was wrong, remove it".
- `merge_branch` is all-or-nothing, and a base twin that moved since the fork
  is reconciled by the same three-way merge `update_page` performs — disjoint
  frontmatter keys merge, the same key on both sides conflicts and the whole
  merge blocks with the branch left open.
- A write naming an unknown or already-decided branch is refused
  (`unknown_branch` / `already_decided`), never silently accepted.

## 0.6.39 — promotion merges a head that moved on other keys

- `create_draft` now records the target's CRDT version as well as its
  `base_sha256`, and `promote_draft` uses it when the target has moved: the
  approval takes the three-way merge path instead of refusing on the byte
  CAS. A reviewer's approval no longer fails because somebody else edited a
  DIFFERENT field of the same page. The response carries
  `auto_merged: true` when it did.
- The same key on both sides still conflicts, the draft stays open, and the
  auto-merged artifact is re-checked against the write guards — a
  `promotable: true` the head gained cannot ride in through a merge nobody
  inspected (ADR-0008).
- `update_page`'s own auto-merge got the same widening: frontmatter changes
  on disjoint KEYS now merge, where before the union had to equal one side
  exactly. Same-key divergence is unchanged — still a conflict.
- With no CRDT backend there is no base snapshot to merge against, and
  promotion behaves exactly as it did.

## 0.6.36 — changesets: a run's held writes, decided together

- `create_draft` gains `new_changeset` (start one; the SERVER mints the id
  and returns it on the stored draft) and `changeset_id` (join the one a
  previous `create_draft` in this run returned). Sending both is refused
  rather than silently resolved — they are two different intentions.
- New tools `list_changesets`, `promote_changeset`, `discard_changeset`
  (`references/02-tool-surface.md`), typed client methods, and
  `escurel changeset list|promote|discard`.
- `promote_changeset` is **all-or-nothing**: every member is checked first
  (still open, still valid, target still at the hash it was drafted
  against) and if any would refuse, NOTHING lands and every member stays
  open to be re-drafted. A half-promoted run leaves the corpus in a state
  no agent proposed, which is the failure this exists to prevent.
- It is also safe to retry: a member whose page already holds its bytes
  counts as applied, so a promotion interrupted mid-flight completes rather
  than conflicting with its own writes, and a changeset that was already
  decided answers `{ok: true, already_decided: true}`.
- **Nothing changes for an ungrouped draft.** `changeset_id` is nullable and
  NULL is today's draft, byte for byte — created, listed, promoted and
  discarded exactly as before, and never listed as a changeset of one.

## 0.6.38 — `diff_draft`: what approving a held write would change

## 0.6.35 — `query_instance` takes `scenario` (corpus traversals)

- A corpus traversal now honours the same `scenario` parameter `expand` /
  `resolve` / `neighbours` / `search` / `list_instances` already take: absent
  reads the base timeline, present reads `base ∪ overlay` with the per-slug
  override, so an overlaid slug appears ONCE and the overlay wins.
- Without this a counting traversal would double-count the moment an overlay
  existed — the failure nobody notices, because the number still looks like a
  number.
- A `sql_view` query ignores it: an external table has no overlay.

## 0.6.34 — stored corpus traversals (`target: corpus`)

- A `[[query::*]]` page may now target the **corpus** instead of a `sql_view`
  instance, declaring a bounded walk over the markdown link graph:
  `traversal: {start, steps: [{relation, direction, as}], where, return,
  max_depth, limit}`. `query_instance` dispatches on `target:`; a `sql_view`
  target is completely unchanged.
- `relation:` is the frontmatter key a link was written under, so nothing new
  is stored — it walks the same links the provenance tools do.
- `max_depth` is mandatory and capped at 12, paths never revisit a page, and
  every hop is ACL-checked per instance: one unreadable hop drops the whole
  path, so a caller sees exactly what they could have reached with
  `neighbours`. That is the property that lets this exist where the old
  `run_stored_query` (arbitrary SQL over the corpus) could not.
- Params are values, never syntax — the only place one reaches is the start
  id, as a bound parameter.
- New validation codes: `traversal_malformed`, `traversal_depth_exceeded`,
  `traversal_unknown_field` (errors) and `traversal_unknown_relation`
  (warning — a relation nothing declares or uses returns nothing, which reads
  like an answer).

## 0.6.33 — typed skill fields: `fields:` constrains what instances may hold

- A skill page may declare `fields:` — the typed counterpart to
  `required_frontmatter`'s key-name list:
  `- {name: hotness, kind: enum, values: [hot, warm, cold]}`. `kind` is the
  closed set `string | int | float | bool | date | datetime | enum | link`.
- A violation is **error**-severity and **rejects the write**, on
  `update_page` and `create_draft` alike: `frontmatter_field_type`,
  `frontmatter_enum_value`, `frontmatter_field_range`. A declared-but-absent
  `required` field reports the existing `frontmatter_required_key_missing`
  code rather than inventing a second vocabulary for one missing key.
- Author-side mistakes are caught on the SKILL page: `fields_malformed`
  (no `name:`, a scalar `fields:`, or `kind: enum` with no `values:` — which
  would enforce nothing) and the warning `field_kind_unknown` (degrades to
  `string`, the same fallback direction `params:` takes).
- `list_skills` rows carry `fields`, so a client builds an instance form from
  the catalogue alone, as it already does a run form from `params`.
- **Opt-in per skill.** A skill with no `fields:` block is completely
  unaffected — declaring the block IS the migration step, which is why
  enforcement can block from the first release without breaking a corpus that
  was written untyped.


## 0.6.31 — server-stamped delegation chain on captured events

- `capture_event` (`references/02-tool-surface.md`): alongside the
  existing server-stamped `provenance.captured_by`, an event captured with
  a **delegated** token now carries `provenance.captured_via` — the
  principal the caller is acting for, taken from the token's RFC 8693
  `act.sub`. Both are gateway claims: a value you send under either key is
  replaced, and `captured_via` is removed outright when the token carries
  no delegation, so its presence always means a real chain.
- Where this comes from: a background run is now packaged with its own
  identity (`sub = agent:<label_skill>`, acting for the runner), so
  `last_written_by` on a page written by a run names the AGENT rather than
  `escurel-runner` (DataZooDE/escurel#510). Consumers reading
  `last_written_by` to attribute an automated write will see agent
  subjects where they previously saw one runner subject.
## 0.6.30 — typed shapes for the rest of the agent tool surface

- `escurel-client` (`references/05-consume-from-rust.md`): typed
  request/response structs + methods for the previously `call_raw`-only
  agent tools — `fetch_blob`, `list_snapshots`, `list_op_authors`,
  `write_instance` (wire key `ref`), and the HTTP session trio
  `open_session`/`apply_op`/`close_session`
  (`CloseSessionRequest::default()` carries the wire default
  `commit: true`). `ExpandResponse` additionally decodes
  `backend_projection` (sql_view/remote live overlay).
- `Error::JsonRpc` gained `data_code`/`retryable`, surfacing the
  gateway's `error.data = {code, retryable}` through the typed client —
  branch on the stable `data_code`, not the numeric JSON-RPC code
  (several refusals share `-32000`). Both `None` when no `data` is sent.

## 0.6.29 — the typed Rust client catches up with the wire guards

- `escurel-client` / `escurel-types` contract parity
  (`references/05-consume-from-rust.md`): `UpdatePageRequest` now
  carries the CAS/approve guards (`base_version`, `require_exact_base`,
  `base_sha256` — `Some("")` = approve-create — and the `provenance`
  passthrough); `UpdatePageResponse` gained `auto_merged` +
  `head_version`/`head_sha256`/`head_content`; `ExpandResponse` gained
  `version` + `content_sha256`, so the full read→hash→guarded-write
  loop is typed end to end. Absent guard = unguarded, byte-identical to
  the old client's wire traffic.
- `ListInstancesRequest.cursor` + client plumbing: `list_instances`
  now resumes from `next_cursor` (only its absence means done).
- The client stopped dropping `as_of`/`scenario` on
  `expand`/`neighbours`/`list_instances` (and `scenario` on `resolve`)
  — they were on the typed requests all along but never reached the
  wire, silently returning current/base state.

## 0.6.28 — chat cursor errors are typed; reader event tools over the lake

- `list_messages` with an undecodable `cursor` now answers
  `invalid_params` (-32602) instead of `internal` (-32603) — parity
  with `list_inbox`/`list_events`, whose garbage-cursor refusal was
  already typed. Treat it like any other bad argument: fix the cursor
  (pass back `next_cursor` verbatim), don't retry.
- A ducklake READER whose event bus is lake-backed
  (`ESCUREL_EVENTS_BACKEND=ducklake`) now serves
  `capture_event`/`assign_event`/`list_events`/`list_inbox` instead of
  rejecting them with `unsupported_on_replica` — the reader gate's
  shared-events probe matched only the Postgres backend. The shared
  surfaces also survive a reader's snapshot refresh now (the hot-swap
  re-applies the chat/events/CRDT attaches).

## 0.6.27 — session write ACL + scoped snapshot history + honest WS resume

- The live-CRDT session surface now enforces `update_page`'s write ACL
  (`ESCUREL_WRITE_ACL`): `open_session` refuses a caller who may not
  write the page (JSON-RPC `-32000`, data code `forbidden`), and
  `close_session(commit: true)` re-checks the same policy at commit
  time — a refusal returns `update_page`'s `{ok: false, issues:
  [{code: "forbidden"}]}` shape and leaves the session open for a
  `commit: false` discard (`references/02`).
- `list_snapshots` is now scoped by the page's read ACL, exactly like
  `list_op_authors`: denial reads as absence — a page you may not read
  reports the same empty history as one that does not exist
  (`references/02`).
- `GET /ws` now applies the tenant suspend gate at the upgrade, matching
  `POST /mcp`: a suspended tenant refuses non-admin bearers with HTTP
  403 before the socket opens; admin still connects.
- `event_subscribe`'s `since_event_id` resume is documented as what it
  is: **best-effort and inbox-only, not gap-free** — the replay reads
  `list_inbox`, so events assigned/processed while disconnected are not
  replayed; reconcile terminal transitions via `list_events`
  (`references/11`). This corrects 0.6.16's "gap-free"/"lossless"
  wording; the wire behaviour is unchanged.

## 0.6.16 — WS gap-free resume + the mode split written down

- `references/11-event-driven-agents.md`: `event_subscribe` accepts
  `since_event_id` (the last id you processed) — the gap replays
  oldest-first with `replayed: true` before the live stream; dedupe by
  `event_id` (an overlap event can arrive twice; duplicates are
  recoverable, gaps are not). Replay window = the most recent 10 000
  inbox rows.
- The WS protocol's two connection modes are now documented as the
  state machine they are (`protocol.md` §WebSocket framing): `hello`
  picks session mode OR presence-only mode irrevocably;
  `event_subscribe` exists only in presence-only mode (session mode
  answers `unknown_frame`); two surfaces = two sockets.
- Token lifetime on long sockets is now specified: auth is at upgrade
  only, a socket outlives its token's `exp`; bound it at the proxy or
  reconnect periodically — which `since_event_id` makes lossless.
## 0.6.26 — `expand` publishes the approve-guard hash

- `expand` (plain reads only — not under `as_of`/`scenario`) now emits
  `content_sha256`: the hash of the STORED markdown bytes, i.e. exactly
  what `update_page`'s `base_sha256` CAS compares against. The
  hold→approve recipe becomes read→hold→guard with no write-probe and
  no byte-perfect reconstruction from parsed fields (heron#30's ask,
  root-caused: `expand` returns parsed frontmatter+body, which cannot
  reproduce the stored bytes).
## 0.6.25 — the chat cursor always makes progress

- `list_messages`' pagination cursor is now built from a full-µs
  timestamp instead of the second-truncated wire `ts` (escurel#406,
  found by the herkules downstream fix): when a page boundary fell
  inside one wall-clock second, `next_cursor` repeated verbatim forever
  and a naive drain loop spun. The wire `ts` is unchanged; only the
  opaque cursor payload gained precision — in-flight old cursors still
  decode (their second-precision value binds, at worst replaying rows
  from the boundary second once; dedupe by `msg_id`).

## 0.6.24 — the typed Rust client carries the pagination cursor

- `escurel-types`: `ListInboxResponse`/`ListEventsResponse` gain
  `next_cursor` and the requests gain `cursor` — the 0.6.10 wire field
  the typed wrapper had silently DROPPED, so no Rust consumer could
  page past the first response (found by the peacock downstream
  audit). `escurel-client` forwards the cursor. Struct literals without
  spread need `..Default::default()`.

## 0.6.23 — codex-review compat: legacy routing + kit contract sync

- The retired `run_stored_query` wire name now ROUTES to
  `query_instance` at dispatch (the `query_id` alias binds, the
  response is a superset) — shipped callers keep working instead of
  hitting method-not-found. The seeded in-tenant meta-skill and
  `platform.md` no longer teach the retired name.
- explorer-kit: `captureEvent` requires a non-empty `labelSkill`
  (matching the 0.6.13 server contract instead of silently sending
  `''` into a `-32602`); `runStoredQuery` is `@Deprecated` and
  delegates to `query_instance`.

## 0.6.22 — an empty `event_id` is refused, not a shared dedup key

- `capture_event` and `POST /ingest*` refuse an empty/whitespace-only
  `event_id` (`-32602` / `422 invalid_event_id`). It is the idempotency
  key, so `""` made EVERY id-less capture the same event — first writer
  wins, every later one silently discarded with a success receipt
  (escurel#390's measured loss). An ABSENT key still mints a server
  ULID; only the explicit empty string was ever affected — and those
  callers were already losing data.

## 0.6.21 — atomic approve: `base_sha256` on `update_page`

- `update_page` gains `base_sha256` — the content-hash CAS that works
  on EVERY gateway (`base_version` needs a CRDT backend; without one it
  answers `versioning_unavailable`, and before this an unknown guard
  arg was silently dropped and the write went through UNGUARDED). Hex
  sha256 of the stored markdown the held write was drafted against;
  `""` = approve-create. A mismatch refuses `{code: conflict}` with
  `head_sha256` + `head_content`; the `base_version` conflict now also
  carries a structured `head_version`. Closes escurel#354 (the
  narrowed approve-atomicity ask): hold `{draft, hash}`, approve via
  the guard, re-diff on conflict — never a silent overwrite.
## 0.6.20 — WS live search: `search_subscribe` is real

- The M3 stub (empty `search_event` ack, ignored payload) is gone:
  `search_subscribe {subscription_id, q, k?, filter?}` runs the real
  ACL-fused hybrid search immediately — the initial `search_event` IS
  the ack, carrying real hits — and re-runs it whenever the index
  mutates, pushing updated hits to the socket. Presence-only
  connections; a missing/empty `q` answers
  `{type: "error", code: "invalid_subscription"}`. Closes the
  live-search half of escurel#355.
## 0.6.19 — verb-first dispatch aliases + scope-keyed quota exemption

- The noun-first stragglers accept verb-first spellings at dispatch:
  `create_tenant`/`list_tenants`/`get_tenant`/`update_tenant`/
  `delete_tenant`/`export_tenant`/`import_tenant` → `tenant_*`, and
  `reload_embedding` → `embedding_reload`. Courtesy aliases only —
  `tools/list` advertises the canonical names.
- Admin-scope tools no longer debit the tenant's *agent* rate budget.
  The exemption keys on the `scope` label (ratcheted against
  `require_admin`) instead of the old `admin_` prefix + hand-kept list,
  which had silently forgotten every unprefixed admin tool
  (`list_credentials`, the pack ops, `create_sql_instance`, …).

## 0.6.18 — surface consolidation: one query tool, two provenance tools

- `run_stored_query` is **removed** (it was admin-gated and already
  DEPRECATED since 0.6.13). `query_instance` is the one query surface;
  it accepts `query_id` as an alias for `ref`, so old argument spellings
  keep working. CLI: `escurel query run` is gone — use
  `escurel query instance`.
- `provenance_path` is **removed as a separate tool**: it is now
  `provenance_ancestry` with an optional `to_page` argument (alias
  `to_page_id`; `page_id` gains aliases `from_page`/`from_page_id`).
  With `to_page` the response is the old `{reachable, path, depth}`
  shape, same fail-closed ACL rule (any private node on the route →
  `reachable: false`, no path); without it the classic `{hops}` walk is
  unchanged.
- `expectation_drift` and `abandoned_paths` are **removed**, consolidated
  into `provenance_report` with input `{kind: "drift" | "abandoned",
  skill?}` returning `{kind, rows}` — the old drift rows for
  `kind: "drift"`, the old abandoned NODES (`{page_id, skill, via}`) for
  `kind: "abandoned"`. Note the key is `rows` for BOTH kinds (the
  abandoned key changed from `nodes`). Unknown kind → `-32602`;
  ACL fail-closed row-dropping unchanged.
- CLI: `escurel provenance drift|abandoned|path` still exist but call
  `provenance_report` / `provenance_ancestry` under the hood (abandoned
  output key changed from `nodes` to `rows`).
- Rust client: `run_stored_query` / `expectation_drift` /
  `abandoned_paths` methods and their `RunStoredQueryRequest/Response`,
  `ExpectationDrift*`, `AbandonedPaths*` types are removed; new
  `provenance_report(ProvenanceReportRequest { kind, skill })`;
  `provenance_path(...)` still exists but calls the consolidated tool;
  `ProvenanceAncestryRequest` gains `to_page`.
- The server now exposes **66 tools** (4 removed, 1 added).
## 0.6.17 — openapi.json documents the whole HTTP surface; outputSchema

- `GET /openapi.json` now describes the REST routes it previously
  omitted — `/ingest`, `/ingest/upload`, `/blob/{page_id}`, `/healthz`,
  `/readyz`, `/version` — with typed request bodies and per-status
  responses, plus `securitySchemes.bearerAuth` (the `/mcp` and REST
  operations declare it).
- `tools/list` entries with a pinned result shape carry an additive MCP
  `outputSchema`: every write tool declares the shared `{ok, issues[]}`
  envelope; `search`/`expand`/`fetch_blob`/`list_*` declare their
  top-level keys incl. the `next_cursor` pagination contract. Tools
  whose results are still ad-hoc stay undeclared rather than lying.
## 0.6.13 — schema ergonomics: required label, aliases, honest envelopes

- `capture_event` now **requires a non-empty `label_skill`** — `{}` used
  to silently mint an event no runner could route (the label selects the
  system prompt). This is the one non-additive change in the series; a
  caller that omitted the label was already broken, just silently.
- Sibling-spelling **aliases** (additive): `list_instances` accepts
  `skill` beside `skill_id`; `search` accepts `skill_id` beside `skill`;
  `move_page` accepts `from_page_id`/`to_page_id` (and
  `from_page`/`to_page`); `export_pack` accepts `pack_id` beside `id`;
  `unsubscribe_pack` accepts `id` beside `pack_id`. Unknown-arg dropping
  can no longer turn a sibling spelling into silent default behaviour.
- Every bare `page_id` input schema now documents the repo-relative
  path format with an example (`markdown/instances/<skill>/<slug>.md`).
- Admin write envelopes that omitted `ok` (`admin_delete_chat_history`,
  `create_sql_instance`, `create_remote_instance`, `unsubscribe_pack`)
  now carry `ok: true`, so their refusal path can reach MCP `isError`.
- `run_stored_query` is marked DEPRECATED in its description — use
  `query_instance`.
## 0.6.15 — `list_instances` cursor pagination (the null era ends)

- `list_instances` accepts an opaque `cursor`, and its `next_cursor` —
  "reserved; always null" since it shipped — finally loads: a string
  while rows lie past the page, `null` on the final page. ONLY null
  means done (the ACL filter legitimately shortens pages). Works under
  every ordering incl. `order_by: "at desc"` with untimed instances,
  and inside scenario overlays (the per-slug winner is chosen over the
  whole set before the cursor cuts). Undecodable cursor → `-32602`.
  Completes review finding R1 across all list surfaces.
## 0.6.14 — `GET /blob/{page_id}`: raw download twin of upload

- New bearer-authed REST route serving a document instance's retained
  original bytes verbatim — declared/sniffed `Content-Type`, honest
  `Content-Length`, no base64 inflation, no 25 MiB cap. Exactly
  `fetch_blob`'s ACL; absent/hidden/blob-less pages are one
  indistinguishable 404. `fetch_blob` stays as the MCP-envelope variant
  for agent callers.

## 0.6.12 — machine-readable error `data: {code, retryable}`

- `references/03-consume-over-http-mcp.md`: JSON-RPC refusals now carry
  an additive `error.data` object with a stable `code` string and the
  `retryable` flag `protocol.md` had promised. Clients branch on
  `data.code` instead of parsing English: `admin_required`,
  `tenant_suspended`, `layer_read_only`, `session_cap_reached`,
  `unknown_session` (reopen, do not back off — it is `-32603` but NOT a
  server fault), `event_not_found` vs `already_assigned` on
  `assign_event`, `read_only_replica` (the retryable one — against the
  writer), `quota_exhausted` (+ `dimension`, `retry_after_ms`),
  `forbidden`, `failed_precondition`, `unsupported_on_replica`,
  `publish_unavailable`. Errors without `data` are unclassified
  internal faults. Fully additive — `code`/`message` unchanged.
## 0.6.11 — `tools/list` is role-scoped; every tool carries `scope`

- Every `tools/list` entry now carries an additive
  `scope: "agent" | "admin"` label, declared at the definition site like
  `execution` and ratcheted against the dispatch arms (a tool cannot
  advertise a scope its gate contradicts).
- **`tools/list` filters by role**: an agent token receives only the
  `scope: "agent"` subset (~28 callable tools) instead of all ~69 — no
  more burning harness context on 41 schemas that can only answer
  `-32001`. Admin tokens and verifier-less dev mode see everything;
  `GET /openapi.json` stays unfiltered.
## 0.6.10 — cursor pagination on `list_inbox` / `list_events`

- `references/02-tool-surface.md`: both event listings accept an opaque
  `cursor` and return `next_cursor` when rows lie past the page — the
  `list_messages` idiom. **Only the absence of `next_cursor` means the
  listing is complete**; a short page never does (the per-event ACL
  filter runs after the limit and legitimately shortens pages). This
  makes an instance's history past `limit` reachable at all — the tail
  used to be permanently silent. An undecodable cursor is `-32602`.
## 0.6.9 — `/ingest` gained dispatch-gate parity + an idempotency key

- `POST /ingest` and `POST /ingest/upload` accept an optional
  `event_id`: the caller's idempotency key, fed to `capture_event`'s
  dedup. A redelivered upload is acknowledged
  `{status: "duplicate", event_id, blob_id}` — one inbox event, no
  second extraction run (escurel#382). Absent = unchanged behaviour.
- The REST intake doors now enforce the same cross-cutting gates as MCP
  dispatch: a suspended tenant refuses agent tokens (403
  `tenant_suspended`), a ducklake reader refuses with 503
  `read_only_replica` (retry against the writer), and `/metrics` records
  the actual response status per route (a refused ingest is no longer
  counted as a 200).
## 0.6.8 — `append_message` `msg_id` is a real idempotency key

- `references/02-tool-surface.md`: a caller-supplied `msg_id` now
  deduplicates — a retry with the same `(chat_group_id, msg_id)` echoes
  the originally stored row (including the server-stamped `ts`) instead
  of inserting a second, microseconds-apart copy. Same contract as
  `capture_event`'s `event_id`. Offline clients can finally retry
  deliveries safely; without a `msg_id` the server still mints a ULID
  and every call appends.
## 0.6.7 — truth pass: discovery, streaming, wire-JSON claims corrected

- `references/03-consume-over-http-mcp.md`: three false claims fixed.
  `tools/list` is NOT role-filtered — every caller sees the whole
  surface (admin tools refuse at dispatch with `-32001`); there is NO
  SSE/streaming on `/mcp` (single JSON bodies only; use the WS
  `event_subscribe` push for wake-ups); JSON-bearing fields
  (`frontmatter`, `rows`, `params`) are real JSON on the wire, not
  `*_json` strings.
- `references/08-auth-and-tenancy.md`: same discovery correction;
  role lists updated (`purge_page` is admin; `admin_delete_chat_history`
  is the real tool name).
- `references/02-tool-surface.md`: `list_instances` row corrected
  (`skill_id` + `order_by='at asc'|'at desc'` — NOT `skill`/`order_by_at`);
  `expand` row corrected (`as_of`/`scenario`/`full`; `anchor`/`version`
  are long gone); `fetch_blob` row added; tool count corrected to 69.
- `expand` now advertises its `full` argument in the input schema (it was
  parsed but undocumented on the wire).
- **New mechanical guard**: `escurel-server/tests/suite/skill_doc_parity.rs`
  reconciles this skill's tool tables (names, input names, the tool count)
  against the live `tools/list` on every test run — the drift class the
  2026-08-02 audit documented can no longer land silently. Prose remains
  a write-time obligation.

## 0.6.6 — WS `event_subscribe`: the bus pushes to open sessions

- `references/11-event-driven-agents.md`: a consumer that cannot host the
  HTTP webhook subscribes over `GET /ws` — `event_subscribe` frame →
  `event_subscribe_ack` → `{type:"event", event:{…}}` pushes for every
  captured event the caller may read (`ESCUREL_EVENT_ACL`-filtered, same
  rule as `list_inbox`); `event_lagged` marks gaps (poll once to catch
  up). Subscription starts at now — no replay. Closes escurel#333.

## 0.6.5 — `assign_event` write-gates its target instance

- `references/02-tool-surface.md`: under `ESCUREL_EVENT_ACL=enforce`,
  `assign_event` now also requires the caller to be allowed to WRITE the
  target instance (`acl.update`), closing the visibility-laundering edge
  escurel#363 — previously only the event side was checked, so a caller
  could file their own capture into someone else's record and re-scope
  its audience. Refusals are `event not found`-shaped (no existence
  oracle); `log` mode warns and allows; a nonexistent target refuses the
  same way. `off` (the default) is unchanged.

## 0.6.4 — a rejected write is flagged via MCP `isError`

- `references/02-tool-surface.md`: a write refused with `ok:false` (a
  validation rejection, a stale `base_version`, a layer/ACL refusal, …)
  now also sets `isError: true` on the MCP `CallToolResult` envelope and
  logs `status: "rejected"` server-side. Previously the envelope said
  `isError: false` and only the nested payload carried the refusal, so a
  client following MCP conventions read it as success. The payload
  contract is byte-identical. `validate` results and `dry_run:true`
  results keep `isError: false` — they report, they don't refuse.

## 0.6.3 — `list_skills` is caller-scoped and no longer discloses group names

- `references/02-tool-surface.md`: the Tier-1 catalogue now filters. A skill
  whose declared `acl.read` does not intersect the caller's effective groups
  is **absent** from `list_skills`, so a consumer asserting "a skill I may
  not read is not in the palette" can now hold that against the server
  instead of re-filtering client side.
- Backward compatible for every tenant that has not opted in: a skill with
  no `acl:` block falls through to the tenant default (`read: [public]`) and
  stays visible, and a skill whose *instances* are owner-private
  (`visibility: owner`, or `acl.read: [owner]`) remains a discoverable
  *type* — `owner` is instance-grained and never hides a skill.
- **Security**: the per-CRUD `acl` object is now projected to an **admin**
  caller only. It previously went to every authenticated caller, which in a
  shared tenant handed out the group names — i.e. the customer roster and
  the authorisation topology — to anyone holding a valid token. `visibility`
  and `owner_field` are unchanged and still reported to everyone.

## 0.6.2 — Live sessions are multi-peer, and attaching is ACL-gated

- `references/02-tool-surface.md`: an `op` from one attached client now
  reaches the others as `peer_op`, and `presence` reaches other peers (live
  cursors). Previously a session was single-peer — a second device learned of
  edits only by asking again — so consumers were being pushed toward building
  their own relay.
- Records the two properties a client must design for: ACL is evaluated at
  attach (a mid-session revocation bites at the next attach), and there is no
  replay of missed frames (`resync_required` → re-`expand` and re-attach).
- **Security**: attaching to a session is now refused for a principal who may
  not read the page. It previously was not checked at all, so any
  authenticated tenant member who knew a session id could watch another
  principal's live editing on a page the ACL denies them.

## 0.6.1 — A committed live session is now readable

- `references/02-tool-surface.md`: `close_session(commit: true)` writes the
  merged body through to the store, so the committed text is immediately
  visible to `expand`, `search` and the link graph, and `final_version` is
  the head a subsequent write should pass as `base_version`.
- Records the failure mode for anyone pinned to an older node: a commit used
  to persist only CRDT history, so `final_version` advanced while the
  readable body did not, and a client writing back with the version it had
  just been handed silently overwrote its own session. The skill said
  nothing about what a commit did to the readable page, which is how a
  consumer could reasonably have assumed the safe behaviour.

## 0.6.0 — Event-driven agents: the runner was entirely undocumented

- New `references/11-event-driven-agents.md`. `escurel-runner` is a
  shipped binary that turns a captured event into an autonomous agent
  run — webhook `POST /trigger` (+ a poll backstop) → loop-control
  ledger → context packaging → a harness adapter spawning `claude -p`
  with the gateway as a scoped MCP server. The skill described only how
  an app CALLS escurel; this is escurel calling an agent, and it had no
  entry at all.
- Documents the consequence for skill authors: an event-triggered
  skill's **page body becomes the agent's system prompt**, so it must
  read as a procedure for a machine, not as reference prose for a human.
- Records the boundary honestly: the runner STARTS a run per event, but
  nothing can push into an already-open session — `/ws` carries no
  event-bus frame (#333), so polling `event inbox` is the only option
  for a live assistant.
- Names the trap that `ESCUREL_RUNNER_HARNESS` defaults to `echo`, so a
  runner started without it runs no LLM at all.

## 0.5.0 — Page layers + skill packs (curated federation)

- **Local iteration corrected (`09`).** The reference claimed the
  workspace built exactly one binary (the CLI) and that no server could
  be run locally; `escurel-server` is a `[[bin]]` and running it is the
  ordinary dev loop. Replaced with three options + a runnable recipe,
  and three traps: `ESCUREL_EMBEDDING_PROVIDER=zero` disables retrieval
  silently, `search` scores are reciprocal-rank fusion (not similarity),
  and the s3/gcs/duckvfs storage backends are cargo features a plain
  `cargo build` silently drops.
- `02`: `delete_page` in the write-tools table, documented as an
  ARCHIVE (retracted from discovery, markdown retained) rather than a
  destroy; plus a note that the tables are curated, not exhaustive.
- `04`: `page delete`, `provenance ancestry|path|drift|abandoned`,
  `workflow run|status|stop`, `ui`, and their CLI→tool map rows.
- `SKILL.md`: a banner stating the skill can be stale and that
  load-bearing details should be checked against a running system. The
  sync obligation is now a PR-cycle step in the repo's `CLAUDE.md`.

- New **Layer/stability axis** in `01`: every page is `overlay`
  (tenant-authored, editable, the default) or `base@<pack>@v<N>`
  (imported from a subscribed skill pack, read-only — `layer_read_only`
  on `update_page`, `-32000 layer_read_only:` on `open_session`; the
  `markdown/base/` page-id namespace is reserved). Overlay **shadowing**
  of a base skill (curator-gated, `shadow_requires_curator`; `resolve`
  prefers the overlay, `list_skills` shows one entry with
  `layer` + `shadows`, `expand` carries a `shadow` object) and the
  curator-set `promotable: true` marker
  (`promotable_requires_curator`).
- New **Skill packs (admin)** section in `02`: `export_pack` /
  `import_pack` / `list_packs` / `rebase_pack` (`rebase_conflict` +
  `acknowledge_conflicts`) / `unsubscribe_pack` / `submit_promotion`
  (default-deny, scrubbed, audit-evented, version-0 candidate), the
  shared-secret HMAC trust model, and the full refusal-code inventory.
- `02` also documents the `execution: "deterministic" | "orchestration"`
  label on every `tools/list` entry (orchestration is the fail-closed
  default).
- `04`: the `escurel admin pack
  export|import|list|rebase|unsubscribe|submit-promotion` subcommands +
  map rows. `06`: the hub↔spoke two-process pack-test recipe
  (`ConfigOverrides.pack_secret`; worked version in
  `crates/escurel-server/tests/pack_import.rs`). `09`: the
  `escurel_writes_total{tenant,origin}` absorption metric.
- `SKILL.md` + `10`: the cross-tenant prohibition re-scoped — runtime
  calls never span tenants; curated pack publish/subscribe is the
  shipped federation layer.

## 0.4.1 — CLI docs realigned to the noun-grouped command surface

- `references/04` rewritten: the CLI is grouped gh/aws-style (`escurel
  skill list`, `escurel page expand`, `escurel query run`, …), not the old
  flat `escurel list-skills` style. Added the four commands that had no
  docs — `page blob` (`fetch_blob`), `page snapshots` (`list_snapshots`),
  `session open|apply|close`, and `ingest` (POST `/ingest/upload`) — plus a
  CLI→tool map, the `--format` flag, stdin-body list, and the create-ACL
  gotcha on `ingest --skill`.
- Noted the **parity guard** (`crates/escurel-cli/tests/cli_parity.rs`):
  every agent-role tool must have a CLI command, so the map can't drift;
  the admin/ops provisioning MCP-twins are deliberately CLI-less.
- Fixed the same stale flat-command style in `references/06`, `07`, `09`.
- `SKILL.md` tool enumeration gains `fetch_blob` + `list_snapshots`.

## 0.4.0 — query pages + the event bus

- `query_instance` documented as THE structured-data read (an authored
  `[[query::<id>]]` page: `{{target}}` allow-list substitution, `:param`
  prepared-statement binding, per-caller ACL on the target, server row
  cap). `run_stored_query` marked legacy.
- New **Event tools** section in `02`: `capture_event` / `assign_event` /
  `list_events` / `list_inbox`, the tenant HMAC webhook, the
  capture+assign-for-timeline rule, and event-id idempotency.
- `create_sql_instance` / `attach_external` documented as post-boot admin
  materialisation (seeding never does it).

## 0.3.0 — External instance backends (SQL views + Document/RAG)

- New **Backend axis** in the data model: a skill may declare
  `backend: { kind: markdown | sql_view | document }`, so its instances are
  sourced from outside markdown. `list_skills` now reports each skill's
  `backend.kind` + a `capabilities` object (`writable`, `granularity`,
  `search`, `supports_crdt`).
  - `sql_view` — read-only DuckDB view over an attached relational source.
    `expand` returns the overlay + a bounded row projection
    (`backend_projection`). Admin-gated lifecycle: `create_sql_instance`,
    `register_credential` / `list_credentials` / `delete_credential`,
    `validate_bindings` (schema-drift → `binding_degraded`, reads fail-closed).
  - `document` — PDF/DOCX/PPTX/XLSX/text uploaded via `POST /ingest` /
    `POST /ingest/upload`, extracted (kreuzberg, default-on) + chunked +
    embedded into a page-with-chunks. `expand` returns the overlay + top-k
    chunks (`chunks_total` / `chunks_truncated`), never the full text.
  - Both backends are read-only: `update_page` / `apply_op` → `backend_read_only`.
- Docs: `references/01` (Backend axis) + `references/02` (Instance backends).
  Full wire contract in the repo's `docs/spec/protocol.md` § Instance backends.

## 0.2.0 — M-Chat: per-chat-group conversation history (issue #63)

- Agent tool surface bumped **12 → 14**. New tools:
  - `append_message(chat_group_id, role, content, [author, ts, metadata,
    msg_id, embed=true])` → `{msg_id, ts}` — append-mostly log keyed
    by an opaque consumer-defined `chat_group_id`. Debits the Writes
    quota. Embedding is opt-out per call.
  - `list_messages(chat_group_id, [since, until, limit=100, cursor,
    direction='desc'])` → `{messages[], next_cursor?}` — time-ordered
    read with half-open `[since, until)` interval and `(ts, msg_id)`
    cursor pagination. Debits Queries.
- Admin RPC: `EscurelAdmin.DeleteChatHistory(tenant_id,
  [chat_group_id, before_ts])` — GDPR right-to-erasure + retention
  pruning. No agent-facing chat-delete tool by design.
- `references/02-tool-surface.md` gains a "Chat tools" section that
  documents the opt-out embedding policy, the opaque `chat_group_id`
  contract, and the admin-only delete path. Pointers from
  `references/03`, `references/05`, `references/08` updated.
- Distinct from `update_page`: chat does **not** rewrite a page or
  embed every block. Routing chat through `update_page` is now an
  explicit anti-pattern.
- ADR: `docs/adr/0002-chat-message-surface.md` in the escurel repo.

## 0.1.0 — initial release

- Progressive-disclosure index over eleven references covering both
  consumption styles (over-the-wire / CLI; the typed Rust path) and both
  emphases (designing the tenant data model; the dev/test loop).
- Conceptual layer: what Escurel is (`references/00`), the skill/instance
  data model + the kind/time/origin axes + the mandatory `escurel`
  meta-skill (`references/01`), and the twelve agent tools
  (`references/02`).
- Consumption paths: MCP-over-HTTP + gRPC (`references/03`), the `escurel`
  CLI (`references/04`), and the Rust `escurel-client` crate
  (`references/05`).
- Dev loop: no-mock integration tests with `escurel-test-support`
  (`references/06`), fixture seeding through the public write path
  (`references/07`), auth/tenancy with `AuthMode`/`mint_token`
  (`references/08`), and local iteration given there is no standalone
  `serve` binary yet (`references/09`).
- Hard prohibitions, the operator/admin boundary, and cross-references to
  `triton-platform` and `substrate-platform` (`references/10`).
- References navigate to the canonical spec under `docs/` and the source
  under `crates/` / `examples/` (resolved through the symlink into the
  Escurel checkout) rather than restating them. No `templates/` — the
  references point at `examples/echo-app/` as the thing to copy.
