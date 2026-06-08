#!/usr/bin/env bash
# examples/agent-runner/demo.sh
#
# End-to-end local demo of the escurel **agent-runner** driving a real agent
# harness to fold an inbound event into a knowledge instance over the
# gateway's MCP-over-HTTP surface. It:
#
#   1. starts the escurel gateway (escurel-server) seeded with this example's
#      minimal corpus, in unauthenticated dev mode (no OIDC);
#   2. starts the agent-runner pointed at the gateway, in a CLEAN working
#      directory (see WHY below);
#   3. captures one real inbox event over /mcp, labelled with the `engagement`
#      skill and pre-flagged to the engagement spine instance;
#   4. waits for the runner's poller to pick it up, run the harness, and fold
#      the event into the instance over /mcp;
#   5. prints the run-ledger row and the folded instance, then tears down.
#
# HARNESS — pick with ESCUREL_DEMO_HARNESS (default: claude):
#   echo    — the reference harness: deterministic, instant, no LLM, no cost.
#             Best for "see the whole pipeline work" and CI-style checks.
#   claude  — the real Claude Code CLI. Needs `claude` on PATH and
#             authenticated (ANTHROPIC_API_KEY or an existing login); consumes
#             Anthropic API quota and takes ~1-2 min per fold.
#
# WHY a clean cwd: the runner spawns `claude` inheriting the runner's working
# directory. If that's a source repo, claude may explore local files instead
# of using the escurel MCP tools. The runner here runs from an empty temp dir
# so the agent's only context is the escurel `/mcp` server.
#
# Usage:
#   examples/agent-runner/demo.sh                      # claude harness
#   ESCUREL_DEMO_HARNESS=echo examples/agent-runner/demo.sh
#   GATEWAY_PORT=18080 RUNNER_PORT=18088 examples/agent-runner/demo.sh
#
# Requires: a built workspace, `jq`, `curl`. (claude harness also: `claude`.)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/examples/agent-runner"
cd "$ROOT"

HARNESS="${ESCUREL_DEMO_HARNESS:-claude}"
GATEWAY_PORT="${GATEWAY_PORT:-18080}"
RUNNER_PORT="${RUNNER_PORT:-18088}"
GW="http://127.0.0.1:${GATEWAY_PORT}"
RN="http://127.0.0.1:${RUNNER_PORT}"
WORK="$(mktemp -d /tmp/escurel-agent-runner.XXXXXX)"
TENANT="default"
TOKEN="demo"                                   # any non-empty string (dev gateway ignores it)
INSTANCE="markdown/instances/engagement__acme-spine.md"
EVENT_ID="EVT-DEMO-$$"
DEADLINE="${ESCUREL_DEMO_DEADLINE:-300}"        # seconds to wait for the fold

note() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }
SERVER_BIN="$ROOT/target/debug/escurel-server"
RUNNER_BIN="$ROOT/target/debug/escurel-runner"
gw_pid=""; rn_pid=""
cleanup() {
  [ -n "$rn_pid" ] && kill "$rn_pid" 2>/dev/null || true
  [ -n "$gw_pid" ] && kill "$gw_pid" 2>/dev/null || true
  wait 2>/dev/null || true
  note "logs + data under $WORK (gateway.log, runner.log)"
}
trap cleanup EXIT

mcp() { # mcp <tool> <arguments-json>  → prints the tool's payload
  curl -fsS "${GW}/mcp" -H "authorization: Bearer ${TOKEN}" -H 'content-type: application/json' \
    -d "$(jq -nc --arg n "$1" --argjson a "$2" \
      '{jsonrpc:"2.0",id:1,method:"tools/call",params:{name:$n,arguments:$a}}')" \
  | jq -r '.result.structuredContent // .result.content[0].text // .result'
}
wait_http() { # wait_http <url> <label> <logfile>
  for _ in $(seq 1 60); do
    curl -fsS "$1" >/dev/null 2>&1 && { note "$2 is up"; return 0; }
    sleep 1
  done
  echo "TIMEOUT waiting for $2 ($1)"; tail -20 "$3" 2>/dev/null; exit 1
}

note "harness=$HARNESS  gateway=$GW  runner=$RN  workdir=$WORK"

note "0/5  build escurel-server + escurel-runner (fast if already built)"
cargo build -q -p escurel-server -p escurel-runner

note "1/5  start gateway — unauthenticated dev mode, example seed corpus"
ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:${GATEWAY_PORT}" \
ESCUREL_SERVER_DATA_DIR="$WORK/data" \
ESCUREL_TENANT="$TENANT" \
ESCUREL_SEED_DIR="$HERE/seed" \
ESCUREL_EMBEDDING_PROVIDER="zero" \
ESCUREL_OBSERVABILITY_METRICS_LISTEN="" \
  "$SERVER_BIN" >"$WORK/gateway.log" 2>&1 &
gw_pid=$!
wait_http "${GW}/healthz" "gateway" "$WORK/gateway.log"

note "2/5  start agent-runner (harness=$HARNESS) from a clean cwd"
mkdir -p "$WORK/runner-cwd"
( cd "$WORK/runner-cwd" && \
  ESCUREL_RUNNER_LISTEN="127.0.0.1:${RUNNER_PORT}" \
  ESCUREL_RUNNER_GATEWAY_URL="$GW" \
  ESCUREL_RUNNER_TENANT="$TENANT" \
  ESCUREL_RUNNER_TOKEN="$TOKEN" \
  ESCUREL_RUNNER_HARNESS="$HARNESS" \
  ESCUREL_RUNNER_POLL_INTERVAL="2s" \
  ESCUREL_RUNNER_LEDGER_PATH="$WORK/runner-ledger.sqlite" \
    "$RUNNER_BIN" >"$WORK/runner.log" 2>&1 ) &
rn_pid=$!
wait_http "${RN}/healthz" "runner" "$WORK/runner.log"

note "3/5  capture an inbox event (label=[[engagement]] → ${INSTANCE})"
mcp capture_event "$(jq -nc --arg id "$EVENT_ID" --arg inst "$INSTANCE" '{
  event_id:$id, source:"manual", mime:"text/plain", label_skill:"engagement",
  instance_page_id:$inst,
  title:"Acme delivery status — call scheduled, change-pool budget query",
  body:"The Acme CTO confirmed the delivery status call for next week and asked to confirm the open T&M change-pool budget before the steering review. Fold this into the engagement spine status."
}')" | jq -c '{event_id, status}'

note "4/5  wait for the runner to fold it (harness=$HARNESS, up to ${DEADLINE}s)"
status=""
for _ in $(seq 1 $((DEADLINE / 2))); do
  row="$(curl -fsS "${RN}/debug/run?tenant=${TENANT}&event_id=${EVENT_ID}" 2>/dev/null || true)"
  st="$(printf '%s' "$row" | jq -r '.status // empty' 2>/dev/null || true)"
  if [ -n "$st" ] && [ "$st" != "pending" ]; then status="$st"; echo "  ledger run: $row"; break; fi
  sleep 2
done

note "5/5  read the folded instance back over /mcp"
mcp expand "$(jq -nc --arg p "$INSTANCE" '{page_id:$p}')" | jq -r '.body' | sed -n '/## Status/,$p'

note "done — event ${EVENT_ID}, terminal run status: ${status:-<timed out>}"
[ "$status" = "processed" ] || {
  echo "Run did not reach 'processed' (got '${status:-timeout}'). See $WORK/runner.log"
  echo "Tip: with the claude harness the agent is non-deterministic; the seed skill"
  echo "     body is its contract — see seed/skills/engagement.md."
  exit 1
}
