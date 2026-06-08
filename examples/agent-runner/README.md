# Agent-runner — connect a real agent (Claude Code) to escurel

This example shows how to run the **escurel agent-runner** end-to-end and
watch a real agent harness fold an inbound event into a knowledge instance
over the gateway's MCP-over-HTTP surface — with **Claude Code** as the agent.

```
 capture_event ──▶  escurel gateway (/mcp)  ◀── agent reads + writes via MCP
                          ▲                          ▲
                          │ poll list_inbox          │ spawns
                    agent-runner ───────────────────▶ harness (claude | echo | …)
                    package → run harness → reconcile (read-back) → ledger
```

The runner is event-driven: an inbox event (webhook or poll) → normalise to a
trigger → idempotency/loop-control gate → **package** the skill body as the
agent's instructions and the gateway `/mcp` as its tool surface → run the
**harness** (a real agent CLI) → **reconcile** by reading back that the event
is `processed` → record in a durable ledger (and optionally cascade).

## TL;DR

```bash
# From the repo root. Builds the two binaries on first run.

# The reference harness — deterministic, instant, no LLM, no cost:
ESCUREL_DEMO_HARNESS=echo examples/agent-runner/demo.sh

# The real Claude Code harness (needs `claude` authenticated; ~1-2 min, uses quota):
examples/agent-runner/demo.sh
```

Both print the ledger run row (`status: processed`) and the folded instance —
its `## Status` and a new dated `## Notes` entry derived from the event.

`demo.sh` is the runnable, verified companion to this doc: it starts a gateway
seeded with [`seed/`](seed), starts the runner, captures one event, waits for
the fold, prints the result, and tears everything down.

## Prerequisites

- A built workspace (`cargo build -p escurel-server -p escurel-runner`), plus
  `jq` and `curl`.
- **For the `claude` harness:** the `claude` CLI on `PATH` and authenticated
  (`ANTHROPIC_API_KEY`, or an existing `claude` login). Live runs consume
  Anthropic API quota and take ~1-2 minutes per fold.
- The gateway must be a build that speaks the **MCP `initialize` handshake**
  and returns MCP-shaped `tools/call` results (`content` + `structuredContent`).
  Older builds exposed only `tools/list`/`tools/call` and **no real MCP client
  could attach** — see [Caveats](#caveats-read-before-you-debug).

## Run it by hand (two processes)

### 1. Gateway — escurel-server (unauthenticated dev mode)

When no OIDC issuer is configured the gateway runs **open**: it ignores the
`Authorization` bearer and serves tenant `default`. That is the local-dev /
on-host story.

```bash
ESCUREL_SERVER_LISTEN_HTTP=127.0.0.1:8080 \
ESCUREL_SERVER_DATA_DIR=/tmp/escurel-data \
ESCUREL_TENANT=default \
ESCUREL_SEED_DIR=examples/agent-runner/seed \
ESCUREL_EMBEDDING_PROVIDER=zero \
ESCUREL_OBSERVABILITY_METRICS_LISTEN= \
  cargo run -p escurel-server
# wait for GET http://127.0.0.1:8080/healthz
```

| Env | Example | Meaning |
|---|---|---|
| `ESCUREL_SERVER_LISTEN_HTTP` | `127.0.0.1:8080` | HTTP listen (serves `/mcp`, `/ws`, `/healthz`) |
| `ESCUREL_SERVER_DATA_DIR` | `/tmp/escurel-data` | host volume for state (default `/data`) |
| `ESCUREL_TENANT` | `default` | the indexer's tenant |
| `ESCUREL_SEED_DIR` | `examples/agent-runner/seed` | markdown corpus to seed (skills + instances) |
| `ESCUREL_EMBEDDING_PROVIDER` | `zero` | no model download |
| `ESCUREL_OBSERVABILITY_METRICS_LISTEN` | *(empty)* | disable the `:9090` metrics port locally |
| `ESCUREL_AUTH_OIDC_ISSUER` | *(unset)* | **unset = open dev mode**; set to enforce JWT auth |

### 2. Agent-runner — escurel-runner

Run it from a **clean working directory** (see [Caveats](#caveats-read-before-you-debug)).

```bash
mkdir -p /tmp/runner-cwd && cd /tmp/runner-cwd
ESCUREL_RUNNER_GATEWAY_URL=http://127.0.0.1:8080 \
ESCUREL_RUNNER_TENANT=default \
ESCUREL_RUNNER_TOKEN=demo \
ESCUREL_RUNNER_HARNESS=claude \
ESCUREL_RUNNER_POLL_INTERVAL=2s \
ESCUREL_RUNNER_LEDGER_PATH=/tmp/runner-ledger.sqlite \
  cargo run -p escurel-runner   # (or the built target/debug/escurel-runner)
# wait for GET http://127.0.0.1:8088/healthz
```

| Env | Example | Meaning |
|---|---|---|
| `ESCUREL_RUNNER_GATEWAY_URL` | `http://127.0.0.1:8080` | gateway base; the runner appends `/mcp` |
| `ESCUREL_RUNNER_TENANT` | `default` | **required** to enable dispatch + poller; match the gateway |
| `ESCUREL_RUNNER_TOKEN` | `demo` | **required**; sent as the gateway bearer AND handed to the harness. Any non-empty string in dev mode; a real JWT against an OIDC gateway |
| `ESCUREL_RUNNER_HARNESS` | `claude` | `echo` (default), `claude`, `codex`, `adk` |
| `ESCUREL_RUNNER_POLL_INTERVAL` | `2s` | inbox poll cadence (default `30s`) |
| `ESCUREL_RUNNER_CLAUDE_BIN` | `claude` | the `claude` binary (override to point at a stub) |
| `ESCUREL_RUNNER_CLAUDE_MODEL` | `opus` | optional `--model` for claude |
| `ESCUREL_RUNNER_LISTEN` | `127.0.0.1:8088` | runner HTTP (`/trigger`, `/debug/*`, `/dlq`, `/metrics`) |
| `ESCUREL_RUNNER_LEDGER_PATH` | `/tmp/runner-ledger.sqlite` | durable run ledger |

(Full set incl. retry/loop-control/quota knobs: `escurel-runner-core/src/config.rs`.)

### 3. Trigger work

**Either** capture an event on the gateway — the poller picks it up within one
interval:

```bash
curl -s http://127.0.0.1:8080/mcp -H 'authorization: Bearer demo' \
  -H 'content-type: application/json' -d '{
  "jsonrpc":"2.0","id":1,"method":"tools/call","params":{
    "name":"capture_event","arguments":{
      "source":"manual","mime":"text/plain","label_skill":"engagement",
      "instance_page_id":"markdown/instances/engagement__acme-spine.md",
      "title":"Acme delivery status — call scheduled",
      "body":"The Acme CTO confirmed the status call and asked about the change-pool budget."
}}}'
```

**Or** inject a trigger straight into the runner (bypasses the poller):

```bash
curl -s http://127.0.0.1:8088/trigger -H 'content-type: application/json' -d '{
  "event_id":"EVT-1","label_skill":"engagement",
  "instance_page_id":"markdown/instances/engagement__acme-spine.md",
  "source":"manual","title":"…","body":"…","tenant_id":"default" }'
# (set ESCUREL_WEBHOOK_SECRET on both sides to require an HMAC signature)
```

### 4. Observe

```bash
curl -s 'http://127.0.0.1:8088/debug/run?tenant=default&event_id=EVT-1' | jq   # one run
curl -s  http://127.0.0.1:8088/debug/ledger | jq                              # status counts
curl -s  http://127.0.0.1:8088/dlq | jq                                       # dead-lettered runs + reason
curl -s  http://127.0.0.1:8088/metrics                                        # prometheus metrics
# read the folded instance:
curl -s http://127.0.0.1:8080/mcp -H 'authorization: Bearer demo' -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"expand","arguments":{"page_id":"markdown/instances/engagement__acme-spine.md"}}}' \
  | jq -r '.result.structuredContent.body'
```

A successful fold ends with the ledger run `status: processed`, the event
`processed` on the instance, and the instance body updated.

## Caveats (read before you debug)

These are the real sharp edges — each one cost a debugging session.

1. **The gateway must be a spec-compliant MCP server.** Claude Code attaches an
   MCP server via an `initialize` handshake and reads `tools/call` results as
   `{content, structuredContent, isError}`. A gateway that only answers
   `tools/list`/`tools/call` with a raw payload → the client **can't connect**
   (gets zero tools) or **reads every tool result as empty**. Both are fixed in
   this build; an older gateway will silently fail to fold.

2. **Run the runner from a clean working directory.** The runner spawns the
   harness inheriting its cwd. Claude Code, started inside a source repo, will
   explore local files instead of using the escurel MCP tools — and then refuse
   or fold the wrong thing. `demo.sh` runs the runner from an empty temp dir.

3. **Skill coherence + the fold contract.** The runner confirms success by
   reading back that the event is `processed` **on the pre-flagged instance**.
   So the event's `label_skill` and its `instance_page_id` must be coherent, and
   **the skill body is the agent's instruction sheet** — it must tell the agent
   to fold into the pre-flagged instance and call `assign_event` on it. If the
   skill's semantics push the agent elsewhere (e.g. an `email` artifact event
   pre-flagged to an `engagement` instance), the agent does something reasonable
   but *different*, read-back never converges, and the run **dead-letters**
   (`/dlq`, reason `retries_exhausted`). See [`seed/skills/engagement.md`](seed/skills/engagement.md)
   for an explicit, working fold contract.

4. **The `claude` harness is non-deterministic, slow, and costs quota.** Expect
   ~1-2 min per fold and Anthropic API usage. For fast, free, deterministic
   iteration use `ESCUREL_DEMO_HARNESS=echo` — the `echo` reference harness does
   the same real `/mcp` fold (`list_inbox`→`expand`→`update_page`→`assign_event`)
   without an LLM.

5. **Auth.** Dev mode (no `ESCUREL_AUTH_OIDC_ISSUER`) accepts any bearer and
   serves tenant `default`. Against an OIDC-enforcing gateway, `ESCUREL_RUNNER_TOKEN`
   must be a valid JWT for that issuer (the runner forwards it to the harness;
   short-TTL per-run scoped-token minting is future work).

## What's in `seed/`

A minimal corpus so the demo is cheap and deterministic (no competing events):

- `skills/engagement.md` — the `engagement` skill, with an explicit
  **"Processing an inbound event"** contract the agent follows.
- `skills/email.md` — a second skill (artifacts → typed instances).
- `instances/engagement__acme-spine.md` — the target engagement spine.
