# Agent-harness orchestration design

**Date:** 2026-06-07.
**Status:** Proposal. High-level design; the epic + sub-issue split
follows from the §"Work-item breakdown" below.
**Scope:** A new standalone component — working name
`escurel-agent-runner` — that turns escurel's M7 inbox into a cascading,
**agent-harness-driven** event→instance projection loop. It triggers a
real agent harness (Claude Code, Codex, Google ADK) on each new inbox
element; the harness files the new information into a new or existing
instance document; that change cascades to trigger further agents, under
explicit loop/safety controls. The harness receives the event's
`label_skill` page as its **instructions** and escurel's `/mcp` surface as
its **toolset** — no bespoke per-harness escurel client.

This is a design proposal, not a spec. It extends the
[`agent-interface.md`](agent-interface.md) contract and the M7
event-sourcing surface ([`../spec/roadmap.md` §M7](../spec/roadmap.md)).

## Why this change

escurel models memory as a triad — **Events · Skills · Instances** — where
an instance's state is the *projection of its event sequence, mediated by
the skills* that describe how to process each event. The M7 surface ships
the plumbing (`events`/inbox table, `capture_event` / `list_inbox` /
`list_events` / `assign_event`, an opt-in outbound capture webhook), but the
*projection* is deliberately left to an external processor, and the
reference one ([`escurel-demo-agent`](../../crates/escurel-demo-agent/))
only folds events via `assign_event` — it never materialises new state, and
there is no cascade. The locked spec principle is **the gateway stays
automation-free**; "event-derived state projection" is explicitly deferred
to v1.5 (roadmap §"Notes on deferred items").

We want: when a new inbox element arrives, escurel triggers a real agent
harness whose task is to file the new information into an instance document;
that write is itself a change that can cascade — re-triggering other agents
to update the same or new documents — until the cascade quiesces. The agent
receives its task as ordinary escurel material: the `label_skill` page is
its instructions, `/mcp` is its toolset.

The outcome is a cascading, harness-driven projection loop with strong
loop/safety controls, **while keeping the gateway automation-free** — all
automation lives in the separate runner.

## Confirmed decisions

1. **Separate runner service** (`escurel-agent-runner`), *not* in-server.
   The gateway stays automation-free; the runner subscribes to the existing
   outbound webhook and/or polls `list_inbox`.
2. **All three harnesses** — Claude Code, Codex, Google ADK — via one
   harness-adapter trait + three concrete adapters.
3. **Cascading is in scope**, with loop controls: idempotency, dedup,
   depth/budget limits, cycle prevention.

## Architecture

New crates, depending only on `escurel-client` + `escurel-types`
(**never** on `escurel-server`/`escurel-index`, so the runner deploys as an
independent process):

| crate | concern |
|---|---|
| `escurel-runner-core` | trigger lifecycle, dispatch queue, cascade emitter, loop-control/ledger, context packager — harness-agnostic engine |
| `escurel-runner-harness` | the `Harness` adapter trait + Claude Code / Codex / Google ADK adapters |
| `escurel-runner` (bin) | deployable process: webhook listener + poller, config (`ESCUREL_RUNNER_*`), graceful shutdown, observability |

Core internal pieces:

1. **Webhook listener** — small axum `POST /trigger` receiving the
   gateway's fire-and-forget event POST; validates a shared secret; returns
   `202` immediately (never blocks the gateway's 5 s timeout); normalises to
   an internal `Trigger`.
2. **Inbox poller** — periodic per-tenant `list_inbox`, the self-healing
   fallback for missed webhooks. Converges with the webhook on one queue;
   dedup collapses the overlap.
3. **Dispatch queue** — bounded per-tenant work queue (backpressure +
   fairness + the natural quota-gating point).
4. **Run ledger** — runner-local durable store (its own SQLite/DuckDB file,
   *not* the tenant store). One row per run; the basis of all loop controls.
5. **Skill/context packager** — turns a `Trigger` into a `TaskContext`:
   `label_skill` → skill page body via `resolve` + `expand` (instructions)
   + event payload + the target instance's current `expand` + `list_events`
   history (input) + the `/mcp` endpoint and a scoped token (toolset
   pointer).
6. **Harness adapters** — spawn the chosen harness with instructions + MCP
   tool config; capture a structured outcome. Adapters do **not** write to
   escurel themselves — writes flow through the harness's own MCP tool
   calls.
7. **Outcome reconciler** — read back to confirm the expected effect (event
   now `processed`, instance version advanced); record the outcome; decide
   retry vs. dead-letter.
8. **Cascade emitter** — after a confirmed successful write, emit a
   follow-on `capture_event` describing the change (the "change → event"
   bridge), tagged with lineage so the next hop is governed.
9. **Observability** — JSON logs + OTel spans, **one trace per cascade
   lineage**; metrics for runs / queue depth / cascade depth. Reuses
   `escurel-obs` patterns.

## Lifecycle (trigger → agent → instance → cascade)

1. External source `capture_event` → inbox (`status=inbox`).
2. Gateway fires the webhook → runner `POST /trigger` (the poller is the
   backstop).
3. Listener normalises → `Trigger { tenant, event_id, label_skill,
   instance_page_id?, lineage }` → enqueue.
4. **Loop-control gate** (ledger): drop if `event_id` already terminal
   (idempotency); drop if in-flight or identical `(instance, content-hash)`
   (dedup); dead-letter if depth ≥ budget or the candidate closes a cycle;
   else create `run_id`, write `pending`, debit the per-tenant run budget.
5. **Context packaging** → `TaskContext` (skill body = instructions, `/mcp`
   + scoped JWT = tools, event + instance state = input).
6. **Harness dispatch** — the adapter spawns the harness; it autonomously
   `search`/`expand`s, drafts, and calls `update_page` + `assign_event`
   through `/mcp`.
7. **Reconciliation** — confirm `processed` + version bump; record outcome.
8. **Cascade emission** — on success, `capture_event` describing the change
   (`source="escurel-runner"`, downstream `label_skill`, lineage in
   `provenance`).
9. The new event re-enters at step 2; the loop-control gate bounds the
   recursion.
10. Terminal: the cascade quiesces, hits the depth budget, or dead-letters.
    The full lineage is one OTel trace.

## "Skills as instructions + tools" delivery

- **Instructions** = the `label_skill` page body, fetched via
  `resolve("[[<label_skill>]]")` → `expand` over `/mcp` — the same surface
  agents already use. The packager prepends a short task framing ("A new
  event of type X arrived: <title/body>. Fold it into the appropriate
  instance per the skill below.") and appends the event payload + current
  instance state + `list_events` history. Optionally it prepends the
  mandatory `escurel` meta-skill for navigation conventions.
- **Toolset** = the gateway `/mcp` endpoint declared as an MCP server in the
  harness's native MCP config, authenticated with a freshly minted
  tenant-scoped, short-TTL JWT (`Role::Agent`). `allowedTools` is narrowed
  to the read tools +
  `validate`/`update_page`/`assign_event`/`capture_event`.

No new escurel surface is needed for delivery — it is `expand` plus each
harness's existing MCP-config mechanism.

## Harness-adapter trait

One async trait in `escurel-runner-harness`: `name()` + `run(&self, task:
&TaskContext) -> Result<HarnessOutcome, HarnessError>`. The adapter must
(1) spawn the harness as a subprocess (isolated per-run dir, timeout,
kill-on-drop), (2) inject the skill markdown as system/instructions,
(3) point the harness at `/mcp` with the bearer header, (4) capture
`HarnessOutcome { status, summary, tool_calls, produced_instance? }`.

The trait deliberately does **not** perform escurel writes itself — writes
flow through the harness's MCP tool calls, keeping "skills as instructions +
tools" honest and the adapter a thin process-management shell.

- **Claude Code CLI** — `claude -p` with `--system-prompt`/`CLAUDE.md`,
  `--mcp-config` (HTTP MCP server → `<gateway>/mcp` + auth header),
  `--allowedTools`, `--output-format json` (the cleanest tool-call
  capture).
- **Codex CLI** — `codex exec` in an isolated dir (mind the full-auto-writes
  gotcha,
  [`../notes/discovered/2026-05-24-codex-full-auto-writes.md`](../notes/discovered/2026-05-24-codex-full-auto-writes.md));
  MCP server via Codex config; coarser tool-call structure → lean on
  read-back.
- **Google ADK** — a thin Python ADK runner script (shipped with the crate)
  building an `Agent` whose instruction is the skill body and whose toolset
  is the escurel MCP server via ADK's `MCPToolset` (streamable-HTTP) →
  `/mcp`; emits a JSON outcome on stdout. The heaviest adapter → behind a
  feature flag.

Adapter selection is per-`label_skill` (a skill may declare its preferred
harness in frontmatter) with a runner-level default — data-driven and
in-corpus, not hardcoded.

## Cascade + loop control (the run ledger)

- **Change → event bridge.** `update_page` emits no event, so the cascade
  emitter calls `capture_event` after a confirmed write. The *runner*, not
  the server, decides a write should cascade → the gateway stays
  automation-free, and hop N+1 reuses the exact hop-0 path.
- **Lineage in `provenance`** (no schema migration needed initially):
  `provenance.runner = { root_event_id, parent_event_id, parent_run_id,
  depth, lineage_path: [...], cause, changed_instance, changed_version }`.
- **Run ledger row:** `run_id, tenant, trigger_event_id, content_hash,
  harness, status (pending|running|succeeded|failed|dead_letter), depth,
  root_event_id, parent_run_id, produced_instance, produced_version,
  attempts, created_at, finished_at, reason`.
- **Controls at the dispatch gate:** idempotency (unique `(tenant,
  trigger_event_id)`); dedup (in-flight + `(instance, content_hash)`);
  depth/budget (`ESCUREL_RUNNER_MAX_DEPTH`, per-root run budget →
  dead-letter `depth_exceeded`); cycle prevention (candidate instance
  already in `lineage_path` → stop `cycle`); per-tenant rate/concurrency
  budget.

## Gateway-side additions (minimal, spec-aligned)

Almost everything is runner-only. The one gateway change needed:

- **Webhook authenticity + tenant identity**
  ([`../../crates/escurel-server/src/webhook.rs`](../../crates/escurel-server/src/webhook.rs)):
  today the POST has no auth header and no explicit tenant. Add (a) a
  configurable HMAC/shared-secret header (`ESCUREL_WEBHOOK_SECRET`) so the
  runner can trust the POST, and (b) the `tenant_id` in the payload/header —
  today the runner cannot tell which tenant the event belongs to. A small
  hardening of an existing opt-in surface; ships with its spec/contract/ADR
  update per M7's "deliberately extending the contract" rule.

Deferred / not recommended: promoting lineage fields to indexed `events`
columns (start with `provenance`); a gateway change-feed for `update_page`
commits (more automation surface — keep the cascade bridge in the runner).

The gateway stays automation-free: it still only *notifies*; the runner
still owns every decision to *act*.

## Concurrency / safety / failure model

- **At-least-once delivery, effectively-once processing** via the ledger's
  unique key + content-hash dedup. No exactly-once pretence (it is not
  achievable across an unreliable webhook + an autonomous harness).
- **Retries** with backoff up to an attempts cap; `assign_event` /
  `update_page` are idempotent enough to converge after a partial success.
- **Dead-letter** for exhausted retries / depth / cycle / unparseable
  output; the originating event is left in the inbox for operator re-drive;
  a DLQ list/requeue path on the runner.
- **Quotas:** per-tenant runs/min + max concurrent runs; a global harness
  subprocess cap (subprocesses are heavy).
- **Auth & tenancy:** a fresh per-run tenant-scoped short-TTL JWT (one
  tenant per token claim, per [`../spec/protocol.md`](../spec/protocol.md));
  an isolated per-run working dir (mandatory for Codex's full-auto write
  behaviour); the webhook secret is the only ingress trust anchor.
- **Graceful shutdown** (SIGTERM drain) + **crash recovery** (durable
  ledger; on restart reconcile `pending`/`running` by read-back against the
  gateway; the poller backstops anything lost).

## Work-item breakdown (proto-epic → future sub-issues)

Each item is a future sub-issue with a one-line scope + the no-mock
integration test that proves it (real `EscurelProcess` + real DuckDB + real
`/mcp`, per [`../../CLAUDE.md`](../../CLAUDE.md) principle 2; a tiny real
"echo harness" stub binary stands in for a live LLM at the boundary — still
no `mockall`).

1. **Runner skeleton + crates** — scaffold the three crates. *Test:*
   release build green; bin starts, binds, answers `/healthz`.
2. **Webhook listener + Trigger** — `POST /trigger`, `202`. *Test:*
   `EscurelProcess` with `ESCUREL_WEBHOOK_URL` → runner; `capture_event`;
   assert the normalised trigger is received.
3. **Gateway webhook auth + tenant identity** (gateway PR) — HMAC secret +
   `tenant_id`. *Test:* runner rejects a wrong-secret POST, accepts the
   correct one, reads the right tenant.
4. **Inbox poller + convergence/dedup** — periodic `list_inbox`. *Test:*
   webhook unset → the poller drives it; both on → exactly one run.
5. **Run ledger + idempotency** — durable ledger; unique `(tenant,
   event_id)`. *Test:* deliver the same event twice → exactly one terminal
   run.
6. **Context packager** — skill-as-instructions assembly. *Test:* seed a
   skill via `FixtureBuilder`; capture a labelled event; assert the
   `TaskContext` has the skill body + event payload + scoped token.
7. **Harness trait + echo adapter** — the `Harness` trait + a real stub
   harness that does a deterministic `update_page` + `assign_event`.
   *Test:* the first true end-to-end — event → instance materialised + event
   `processed`.
8. **Claude Code adapter** — `claude -p` + `--mcp-config` +
   `--output-format json`. *Test:* env-guarded; assert the expected `/mcp`
   tool calls against the real gateway.
9. **Codex adapter** — `codex exec` in an isolated dir. *Test:* env-guarded;
   instance materialised; no stray files outside the run dir.
10. **Google ADK adapter** — Python ADK runner + `MCPToolset` → `/mcp`,
    feature-flagged. *Test:* env-guarded; fold via real `/mcp`.
11. **Outcome reconciler + read-back + retry** — confirm the effect; retry
    on transient failure. *Test:* inject a transient `/mcp` failure → retry
    then success in the ledger.
12. **Cascade emitter + lineage** — `capture_event` on success with
    lineage. *Test:* a meeting → decision-record chain; assert a second run
    fires with `provenance.runner.depth == 1` + the correct `root_event_id`.
13. **Loop controls (depth/budget/cycle)** — enforce at the dispatch gate.
    *Test:* an A→B→A cascade → stops at the depth limit / cycle detection;
    lands in `dead_letter` with the correct reason.
14. **DLQ + quotas + observability + graceful shutdown** — hardening.
    *Test:* exceed runs/min → throttled; one cascade → a single OTel trace
    across hops; SIGTERM mid-run → drain + ledger consistency on restart.

**Sequencing:** 1→2→3→4→5 (ingest + dedup foundation) → 6→7 (core fold,
first green) → 8/9/10 in parallel (the three real adapters) → 11→12→13
(cascade + safety) → 14 (hardening).

## Critical files (for the implementer)

- [`../../crates/escurel-demo-agent/src/lib.rs`](../../crates/escurel-demo-agent/src/lib.rs)
  — the `McpClient` + `process_inbox_once` pattern the runner core extends
  (fold-only → harness materialisation).
- [`../../crates/escurel-server/src/webhook.rs`](../../crates/escurel-server/src/webhook.rs)
  — the trigger source; needs the auth + tenant-identity addition (item 3).
- `../../crates/escurel-index/src/events.rs` —
  `capture_event`/`assign_event`/`list_inbox`/`list_events`; `provenance` is
  the lineage carrier.
- `../../crates/escurel-types/src/events.rs` — the `Event` /
  `CaptureEventRequest` wire types the runner (de)serialises.
- `../../crates/escurel-test-support/src/lib.rs` — the no-mock harness every
  acceptance test builds on.
- [`../spec/roadmap.md`](../spec/roadmap.md),
  [`agent-interface.md`](agent-interface.md) — the spec/contract to extend.

## Open follow-up

This design introduces orchestration automation that the current spec
explicitly defers to v1.5 ("event-derived state projection"). We keep the
**gateway** automation-free by housing all automation in the separate
runner, but the roadmap/contract should be updated to record that the
projection now exists *as an external runner* (one small doc PR alongside
item 3), so the spec and the code don't diverge.
