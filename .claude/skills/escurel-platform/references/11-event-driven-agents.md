# 11 — Event-driven agents (`escurel-runner`)

The other three references answer *"how does my app call escurel?"*. This
one answers the inverse: **how does escurel call an agent?**

`escurel-runner` (`crates/escurel-runner`) is a real, shipped binary that
turns a captured event into an autonomous agent run. It is what lets a
locally-running assistant — Claude Code, an IDE assistant, a CLI worker —
act on the knowledge base without you driving it by hand.

Canonical design: `docs/contract/agent-orchestration.md`.

## What it actually does

```
capture_event ─▶ gateway fires the HMAC webhook
                       │
                       ▼
              runner  POST /trigger          (poller is the backstop)
                       │
                       ▼
              loop-control gate (the run ledger)
                       │
                       ▼
              context packaging → TaskContext
                       │
                       ▼
              harness adapter spawns `claude -p`
                       │  the agent autonomously search/expand/update_page
                       │  /assign_event over /mcp under a scoped token
                       ▼
              reconcile ─▶ cascade: capture_event for the next hop
```

The crucial design point, and the reason this is not "escurel shelling out
to an LLM": **the adapter performs no escurel writes.** It is process
management, invocation construction and outcome capture, nothing more.
Every effect flows through the agent's own `/mcp` tool calls under a
freshly minted, tenant-scoped, short-TTL `Role::Agent` JWT.

## Skills as instructions, `/mcp` as tools

This is the part worth internalising if you author skills:

- **Instructions** = the triggering event's `label_skill` **page body**,
  fetched with `resolve` → `expand`. The packager frames it ("A new event
  of type X arrived… fold it into the appropriate instance per the skill
  below"), then appends the event payload, current instance state and
  `list_events` history.
- **Tools** = the gateway's `/mcp`, declared as an MCP server in the
  harness's native config, with `allowedTools` narrowed to the read tools
  plus `validate` / `update_page` / `assign_event` / `capture_event` —
  and `report_progress` on every packaging (review runs and workflow
  steps too). When the run's token can report — a **minted**, run-bound
  bearer, i.e. every production run — the instructions end with a
  paragraph telling the agent to report its whole plan up front and on
  every step change, which the gateway files as `run-progress` events for
  the humans watching the run. A static-bearer dev runner packages without
  the paragraph: `report_progress` refuses a token with no run claim, and a
  model told to call a tool that always refuses burns its turns on it.

**Consequence for skill authors:** any skill that can be event-triggered
is read by a machine as its system prompt. Write those skill bodies as a
**procedure for an agent**, not as reference documentation for a human. A
skill body that merely describes a type produces an agent with no idea
what to do.

No new escurel surface is involved — it is `expand` plus each harness's
existing MCP-config mechanism.

## Harness adapters

`crates/escurel-runner-harness/` implements the `Harness` trait once per
CLI:

| adapter | selector | drives |
|---|---|---|
| `claude.rs` | `ESCUREL_RUNNER_HARNESS=claude` | `claude -p` headless CLI (#152) |
| `codex.rs` | `…=codex` | OpenAI Codex CLI |
| `adk.rs` | `…=adk` | Google ADK |
| `echo.rs` | `…=echo` (**default**) | deterministic no-LLM stub |

The Claude adapter runs `claude -p` as an isolated, timed, kill-on-drop
subprocess: registers the gateway via `--mcp-config`, narrows the surface
with `--allowedTools mcp__escurel__<tool>`, injects the skill body as the
appended system prompt, and parses the `--output-format json` envelope.
Default per-run timeout 300 s.

`ESCUREL_RUNNER_CLAUDE_BIN` (default `claude`) exists so a deterministic
test can point at a stub executable that mimics the CLI's I/O contract —
exercising invocation-build and parse without burning quota.

## Running it locally

```sh
cargo build -p escurel-runner

ESCUREL_RUNNER_GATEWAY_URL=http://127.0.0.1:8080 \
ESCUREL_RUNNER_TENANT=<tenant> \
ESCUREL_RUNNER_TOKEN=<agent bearer, or omit against an unauthenticated dev gateway> \
ESCUREL_RUNNER_HARNESS=claude \
  ./target/debug/escurel-runner
```

Then capture an event and watch it drive a run:

```sh
escurel event capture --source local --mime text/plain \
  --label-skill note --title "something happened"
```

Key settings (full list in `crates/escurel-runner-core/src/config.rs`):

| var | default | meaning |
|---|---|---|
| `ESCUREL_RUNNER_LISTEN` | `0.0.0.0:8088` | its own HTTP surface |
| `ESCUREL_RUNNER_GATEWAY_URL` | `http://127.0.0.1:8080` | the escurel gateway |
| `ESCUREL_RUNNER_HARNESS` | `echo` | **`echo` is the default — set `claude` or nothing runs an LLM** |
| `ESCUREL_RUNNER_CLAUDE_BIN` | `claude` | binary path (or a test stub) |
| `ESCUREL_RUNNER_POLL_INTERVAL` | `30s` | inbox-poll backstop |
| `ESCUREL_RUNNER_EMIT_EVENTS` | `true` | write each run's lifecycle as `escurel:run` system events (see *Run lifecycle events*); `false` = the workbench sees events and drafts but no runs |
| `ESCUREL_RUNNER_MAX_DEPTH` | `8` | cascade depth budget |
| `ESCUREL_RUNNER_TENANT_MAX_CONCURRENT`, `…_RUNS_PER_MIN` | — | per-tenant limits |

Its routes: `/healthz`, `/version`, `/metrics`, `POST /trigger`, `/dlq`,
`/dlq/requeue`, and `/debug/{seen,ledger,run}`.

`POST /trigger` takes an **optional shared secret**; when set, the request
must carry a valid HMAC-SHA256 signature of the body — the same signature
the gateway's outbound webhook produces.

## Two ingress paths, one queue

Webhook **and** poller converge on one dedup queue, so neither duplicates
the other's work. The **run ledger** is the idempotency authority: it drops
an `event_id` already terminal, drops an in-flight or identical
`(instance, content-hash)`, and dead-letters anything past the depth budget
or closing a cycle. That is what makes cascades safe — a run emits a
`capture_event` for the next hop, which re-enters at the same gate.

## Event kinds and lineage

Every event carries three more fields on the wire: `kind`, `root_event_id`,
`run_id`.

- **`kind: user`** (the default) is work — something happened that a skill
  should fold into an instance. Everything above is about user events.
- **`kind: system`** is bookkeeping ABOUT a run, written by the runner and
  the gateway under the reserved `escurel:` labels (`escurel:run` for a
  run's lifecycle, `escurel:review` for draft transitions, `escurel:runner-status`
  for runner health; the pre-existing `escurel:run-status` is a workflow
  *operation's* status and stays as it is). Admin-only to capture — a forged
  `run-finished` would be a forged run. A system event captured with an
  `instance_page_id` is stored `processed` on that page at once (no
  `assign_event`: it was never inbox work); without one it sits unassigned.
  **The runner never dispatches a system event, nor anything under an
  `escurel:` label** — its gate drops both before a ledger row exists, on
  the webhook path as well as the poll path — and `list_inbox` /
  `list_events` hide them unless you pass `include_system: true`, so a
  consumer that never asks sees exactly what it saw before.
- **`root_event_id`** is the lineage root. A user event that names none is
  its own root; a cascade hop and a run event inherit it from
  `provenance.runner.root_event_id` at capture (server-side — there is no
  argument for it). `list_events { root_event_id }` is therefore the whole
  thread under a root: the root itself, its cascade hops, and — with
  `include_system` — its runs, any status, oldest first.
- **`run_id`** is the run a system event belongs to (from
  `provenance.runner.run_id`). A cascade hop is *emitted by* a run and names
  it as `provenance.runner.parent_run_id`; it does not carry the run's id
  itself. `list_events { run_id }` is one run's own events and implies
  `include_system`.

## Run lifecycle events (`escurel:run`)

Every run the runner admits is written back to the gateway as three kinds
of `kind: system` events under `label_skill: escurel:run`, attached to the
run's target page (`processed`, never inbox work) and readable with
`list_events { run_id }` or `list_events { root_event_id, include_system }`:

| title | when | body |
|---|---|---|
| `run-started` | the ledger admitted the run and a harness is about to run it | `{}` |
| `run-attempt` | each try ends | `{attempt, started_at, ended_at, outcome: ok \| converged \| failed \| timeout, error?}` |
| `run-finished` | the ledger reached its terminal | `{status: processed \| failed \| dead_letter, reason?, attempts, held, summary, tool_calls, produced_instance, produced_version, plan}` — `plan` is the newest `run-progress` snapshot the agent reported |

`provenance.runner` on each carries `run_id`, `root_event_id`, `event_id`
(the trigger), `parent_run_id` (a cascade hop's emitting run), `depth`,
`lineage_path`, `trace_id`, `harness`, `max_attempts`, `attempt`,
`target_page_id` and, on `run-finished`, `autonomy`. Event ids are
deterministic (`run:<run_id>:started` / `:attempt:<n>` / `:finished`), so a
re-emission is idempotent. **The ledger stays the source of truth**: these
are its projection and are best-effort — a gateway that refuses them (a
non-admin runner bearer cannot write the `escurel:` namespace) is logged
and counted (`escurel_runner_run_events_failed_total{kind}`), and the run
lands regardless. A run whose process died before recording its terminal
is reconciled on the next boot, and its `run-finished` is written then
(`provenance.runner.harness: recovery`, `attempts: 0`) — so a terminal the
ledger reached is never missing from the projection.

## Review events (`escurel:review`)

Every held-write transition is a `kind: system` event under
`label_skill: escurel:review` on the draft's target page, so a review queue
and a lineage tree update live from the bus instead of polling
`list_drafts`:

| title | when |
|---|---|
| `draft-created` | `create_draft` held a write (also for the runner's per-run agent) |
| `draft-promoted` | `promote_draft` landed it (`already_decided: true` when the bytes had already landed and the decision merely completed) |
| `draft-discarded` | `discard_draft`, or a stale predecessor superseded by a re-draft (`body.reason` says which) |
| `changeset-promoted` / `changeset-discarded` | the whole-changeset decision, after its per-member events |
| `changeset-already_decided` | a retried decision on a changeset already decided |

`provenance.review` carries `{draft_id, changeset_id, run_id,
root_event_id, decided_by, already_decided}`. The run lineage comes from
the draft ROW (what the runner signed into it), never from the caller.
Best-effort: a decision never fails because the bus could not be told.
Hidden from the default list surfaces like every system event; pushed to
`event_subscribe` subscribers like every event.

## Reading a lineage: `list_lineage`

`list_lineage { root_event_id }` returns the whole thread under a root
event as a flat list of nodes for you to fold into a tree:

| type | id | parent | state | carries |
|---|---|---|---|---|
| `event` | the event id | the run that emitted it (`provenance.runner.parent_run_id`); `null` for the root | `inbox` / `processed` | `label_skill`, `title`, `at`, `kind`, `instance_page_id`, `parent_event_id`, `depth` |
| `run` | the run id | the event that triggered it | `running` until its `run-finished`, then `processed` / `failed` / `dead_letter` | `harness`, `attempt`, `max_attempts`, `started_at`, `finished_at`, `summary`, `produced_instance`, `plan` (newest `run-progress`), `autonomy`, `target_page_id` |
| `changeset` | the changeset id | the run that proposed it | `open` / `promoted` / `discarded` / `mixed` | `drafts`, `author` |
| `draft` | the draft id | its changeset, else its run | `open` / `promoted` / `discarded` | `target_page_id`, `author`, `decided_by`, `event_id` |

Runs are folded from their `escurel:run` rows; `escurel:review` rows are
not nodes (the drafts carry that state). A draft whose run wrote no events
(a static-bearer dev runner) hangs off the root, its `run_id` still set.
ACL fails closed per node: an unreadable node is absent with its subtree,
and an unreadable or unknown root is an empty tree — never an error. The
events half is paged (`limit`, default 500; `next_cursor`), the drafts half
is not; nodes are keyed by id, so merge pages by id. Then subscribe with
`event_subscribe { filters: { root_event_id } }` for the deltas.

## Watching the bus from an open session: `event_subscribe`

An agent that cannot host the HTTP webhook (a locally-running assistant,
an IDE process behind NAT) subscribes over the WebSocket instead of
polling (#333, shipped):

1. `GET /ws` with the bearer, hello `{ "type": "hello", "presence_only": true }`;
2. send `{ "type": "event_subscribe", "subscription_id": "<yours>" }`,
   await `{ "type": "event_subscribe_ack", ... }`;
3. every event captured from then on that THIS caller may read arrives as
   `{ "type": "event", "subscription_id": ..., "event": { ...same shape
   as list_inbox rows... } }`. Filtering follows `ESCUREL_EVENT_ACL`
   exactly like the polling surfaces (`off` = open bus, `enforce` = only
   events `may_read_event` allows you).
4. on `{ "type": "event_lagged", "skipped": n }` the push stream has
   gaps — poll `list_inbox` once to catch up, keep the subscription.

**Filters.** `{ "type": "event_subscribe", "subscription_id": …,
"filters": { "root_event_id": "<root>" } }` narrows the push server-side
(before the per-event ACL) to one lineage; `run_id`, `label_skill`,
`kind` (`user` | `system`) and `instance_page_id` work the same way and
combine. A workbench watching a thread therefore gets exactly that
thread — its cascades, its runs' `escurel:run` rows, its `escurel:review`
transitions — and nothing else. A malformed filter answers
`{ "type": "error", "code": "invalid_subscription" }` and subscribes
nothing.

**Resume.** With a `root_event_id` or `run_id` filter, `since_event_id`
replays from the lineage's own event log — any status, system rows
included — so a run event stored `processed` while you were away is
replayed too (gap-free for the thread; dedupe by `event_id`, run events
carry non-ULID ids and replay on every resume). Without a lineage filter,
pass `since_event_id` (the last event id you processed) on the
subscribe frame to resume: the still-**inbox** events after that id
replay oldest-first with `replayed: true` before the live stream —
dedupe by `event_id`, an overlap event can arrive twice. This resume is
**best-effort and inbox-only, NOT gap-free**: the replay reads
`list_inbox`, so an event that was assigned/processed while you were
disconnected has left the inbox and is not replayed — reconcile
terminal transitions via `list_events` if you must not miss them.
Without `since_event_id` the subscription starts at now (no replay).
`escurel event inbox --limit 50` poll for anything captured before the
ack. The runner path is unchanged — **runner = event starts a new agent
run**; **event_subscribe = an open session watching the bus live**.
`search_subscribe` remains a stub (ACKs `hits: []`; live search push is
still v1-deferred, issue #355).
