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
  plus `validate` / `update_page` / `assign_event` / `capture_event`.

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

## What this does NOT give you

**An event cannot be pushed into an agent session that is already open.**
The runner *starts* a process per event; it has no channel into a
long-lived conversation. A locally-running assistant that wants to observe
the bus while working must poll:

```sh
escurel event inbox --limit 50
```

`GET /ws` exists (frames: `hello`, `presence`, `search_subscribe`, `op`,
`peer_op`, `resync_required`, `close`) but carries **no event-bus frame** —
tracked as issue #333. Session ops *do* now fan out to every attached peer
(#352); that is document co-editing, not the event bus.
`search_subscribe` is not a substitute: it ACKs with `hits: []` and live
push is v1-deferred.

So: **runner = event starts a new agent run** (works, shipped);
**polling = an open session watching the bus** (the only option today).
