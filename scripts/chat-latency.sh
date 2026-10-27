#!/usr/bin/env bash
# Cold chat latency (governed chat agent, BRD NFR-P2): ask one 3-tool question
# N times on a given harness and record, per run, the time from the `chat`
# capture to the run's first `report_progress` and to the assistant's answer.
# Every run spawns a fresh harness process, so every run is cold.
#
#   scripts/chat-latency.sh claude 3
#   GEMINI_API_KEY=… scripts/chat-latency.sh gemini 3
#
# Stack: escurel-test-gateway (verifying) seeded with the CRM demo corpus plus
# the chat skills, and escurel-runner in minted mode (poll intake, 250 ms).
# Prints one TSV row per run: harness, run, first_progress_s, answer_s, status.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HARNESS="${1:?harness (claude|gemini)}"
RUNS="${2:-3}"
fail() { echo "FAIL: $*" >&2; exit 1; }
for c in cargo curl jq sha256sum; do command -v "$c" >/dev/null || fail "$c not on PATH"; done

cargo build -q --release -p escurel-test-support --bin escurel-test-gateway \
  && cargo build -q --release -p escurel-runner --bins || fail "cargo build"
BIN="$ROOT/target/release"
HOME_DIR="$(mktemp -d)"
mkdir -p "$HOME_DIR/seed"
cp -r "$ROOT/examples/crm-demo/skills" "$ROOT/examples/crm-demo/instances" "$HOME_DIR/seed/"
cp "$ROOT"/examples/chat/seed/skills/*.md "$HOME_DIR/seed/skills/"
# The gateway ships its own `escurel` meta-skill; the demo's older copy is refused.
rm -f "$HOME_DIR/seed/skills/escurel.md"
"$BIN/escurel-test-gateway" --tenant lat --seed "$HOME_DIR/seed" --subject latency \
  >"$HOME_DIR/gateway.out" 2>"$HOME_DIR/gateway.log" &
GW=$!
trap 'kill "$GW" "${RUNNER:-}" >/dev/null 2>&1 || true' EXIT
for _ in $(seq 1 240); do [[ -s "$HOME_DIR/gateway.out" ]] && break; sleep 0.5; done
head -n 1 "$HOME_DIR/gateway.out" >"$HOME_DIR/conn.json"
cg() { jq -r ".$1" "$HOME_DIR/conn.json"; }
URL="$(cg gateway_url)"
BEARER="$(cg bearer)"
[[ -n "$URL" && "$URL" != null ]] || fail "gateway did not start ($HOME_DIR)"
env -u ESCUREL_RUNNER_TOKEN \
  ESCUREL_RUNNER_GATEWAY_URL="$URL" ESCUREL_RUNNER_TENANT="$(cg tenant)" \
  ESCUREL_RUNNER_AUTH_ISSUER="$(cg issuer_url)" ESCUREL_RUNNER_AUTH_KID="$(cg kid)" \
  ESCUREL_RUNNER_AUTH_SIGNING_KEY="$(cg signing_key)" ESCUREL_RUNNER_HARNESS="$HARNESS" \
  ESCUREL_GEMINI_API_KEY="${ESCUREL_GEMINI_API_KEY:-${GEMINI_API_KEY:-}}" \
  ESCUREL_RUNNER_LISTEN=127.0.0.1:0 ESCUREL_RUNNER_LEDGER_PATH="$HOME_DIR/ledger.duckdb" \
  ESCUREL_RUNNER_POLL_INTERVAL=250ms \
  "$BIN/escurel-runner" >"$HOME_DIR/runner.log" 2>&1 &
RUNNER=$!
mcp() {
  jq -nc --arg name "$1" --argjson args "$2" \
    '{jsonrpc: "2.0", id: 1, method: "tools/call", params: {name: $name, arguments: $args}}' \
    | curl -fsS -X POST "$URL/mcp" -H "authorization: Bearer $BEARER" \
        -H 'content-type: application/json' --data-binary @-
}
now() { date +%s.%N; }
SUB="$(mcp whoami '{}' | jq -r '.result.structuredContent.subject')"
PAGE="markdown/instances/conversation/c-latency.md"
GROUP="page-chat:v1:$(printf %s "$SUB" | sha256sum | cut -d' ' -f1):$PAGE"
DOC="$(printf -- '---\nkind: instance\nskill: conversation\nid: c-latency\ntitle: latency\nowner: "%s"\nstarted_at: 2026-10-27T00:00:00Z\n---\n# latency\n' "$SUB")"
mcp update_page "$(jq -nc --arg p "$PAGE" --arg c "$DOC" '{page_id: $p, base_sha256: "", content: $c}')" >/dev/null
QUESTION="Which customers do we have, and which of them has an open opportunity? Cite the pages you used."
printf 'harness\trun\tfirst_progress_s\tanswer_s\tstatus\n'
for n in $(seq 1 "$RUNS"); do
  msg="lat-$n"
  mcp append_message "$(jq -nc --arg g "$GROUP" --arg m "$msg" --arg q "$QUESTION" \
    '{chat_group_id: $g, role: "user", content: $q, msg_id: $m, embed: false}')" >/dev/null
  t0="$(now)"
  ev="$(mcp capture_event "$(jq -nc --arg g "$GROUP" --arg p "$PAGE" --arg m "$msg" \
    '{label_skill: "chat", source: "latency", instance_page_id: $p, title: "chat turn",
      provenance: {chat_group_id: $g, user_msg_id: $m}}')" | jq -r '.result.structuredContent.event_id')"
  progress="" answer="" status=""
  while :; do
    elapsed="$(echo "$(now) - $t0" | bc)"
    if [[ -z "$progress" ]] && mcp list_events "$(jq -nc --arg r "$ev" '{root_event_id: $r, include_system: true}')" \
        | jq -e '.result.structuredContent.events[] | select(.title == "run-progress")' >/dev/null 2>&1; then
      progress="$elapsed"
    fi
    status="$(mcp list_messages "$(jq -nc --arg g "$GROUP" '{chat_group_id: $g, direction: "asc"}')" \
      | jq -r --arg m "$msg" '.result.structuredContent.messages[]
          | select(.role == "assistant" and .metadata.user_msg_id == $m) | .metadata.status' 2>/dev/null)"
    if [[ -n "$status" ]]; then answer="$elapsed"; break; fi
    (( $(echo "$elapsed > 300" | bc) )) && { status="timeout"; break; }
    sleep 0.25
  done
  printf '%s\t%s\t%s\t%s\t%s\n' "$HARNESS" "$n" "${progress:--}" "${answer:--}" "$status"
done
echo "logs: $HOME_DIR" >&2
