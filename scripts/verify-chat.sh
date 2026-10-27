#!/usr/bin/env bash
# The governed chat agent, end to end (BRD AC9): a question is ANSWERED by a
# run. A chat answer is posted with the run's own token, and only a gateway
# that VERIFIES tokens can tell a run's token from anyone's — so this starts
# its own stack: escurel-test-gateway (the Rust suites' issuer) seeded with
# the chat corpus (examples/chat/seed), plus escurel-runner in minted mode on
# the echo harness, and drives one question through /mcp as the signed-in
# person: whoami → create a conversation → append the turn → capture the
# `chat` event → poll list_messages for the assistant's answer.
#
# Called by scripts/verify-demo.sh; runs on its own too (needs cargo, curl, jq).
# Exit 0 = the run answered. Logs stay in the printed directory on failure.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
note() { echo ">>> $*"; }
for c in cargo curl jq sha256sum; do command -v "$c" >/dev/null || fail "$c not on PATH"; done

note "chat: building escurel-test-gateway + escurel-runner"
cargo build -q -p escurel-test-support --bin escurel-test-gateway \
  && cargo build -q -p escurel-runner --bins \
  || fail "cargo build chat stack"
CHAT_HOME="$(mktemp -d)"
"$ROOT/target/debug/escurel-test-gateway" --tenant chat --seed "$ROOT/examples/chat/seed" \
  --subject verify-demo --write-acl-enforce >"$CHAT_HOME/gateway.json" 2>"$CHAT_HOME/gateway.log" &
CHAT_GW_PID=$!
for _ in $(seq 1 120); do [[ -s "$CHAT_HOME/gateway.json" ]] && break; sleep 0.5; done
[[ -s "$CHAT_HOME/gateway.json" ]] || fail "chat gateway did not start (see $CHAT_HOME/gateway.log)"
# The connection line is the FIRST line; the gateway's own logs follow it on stdout.
head -n 1 "$CHAT_HOME/gateway.json" >"$CHAT_HOME/connection.json"
cg() { jq -r ".$1" "$CHAT_HOME/connection.json"; }
CG_URL="$(cg gateway_url)"
CG_BEARER="$(cg bearer)"
env -u ESCUREL_RUNNER_TOKEN \
  ESCUREL_RUNNER_GATEWAY_URL="$CG_URL" ESCUREL_RUNNER_TENANT="$(cg tenant)" \
  ESCUREL_RUNNER_AUTH_ISSUER="$(cg issuer_url)" ESCUREL_RUNNER_AUTH_KID="$(cg kid)" \
  ESCUREL_RUNNER_AUTH_SIGNING_KEY="$(cg signing_key)" ESCUREL_RUNNER_HARNESS=echo \
  ESCUREL_RUNNER_LISTEN=127.0.0.1:0 ESCUREL_RUNNER_LEDGER_PATH="$CHAT_HOME/ledger.duckdb" \
  ESCUREL_RUNNER_POLL_INTERVAL=250ms \
  "$ROOT/target/debug/escurel-runner" >"$CHAT_HOME/runner.log" 2>&1 &
CHAT_RUNNER_PID=$!
trap 'kill "$CHAT_GW_PID" "$CHAT_RUNNER_PID" >/dev/null 2>&1 || true' EXIT
chat_mcp() {
  jq -nc --arg name "$1" --argjson args "$2" \
    '{jsonrpc: "2.0", id: 1, method: "tools/call", params: {name: $name, arguments: $args}}' \
    | curl -fsS -X POST "$CG_URL/mcp" -H "authorization: Bearer $CG_BEARER" \
        -H 'content-type: application/json' --data-binary @-
}
chat_sub="$(chat_mcp whoami '{}' | jq -r '.result.structuredContent.subject')"
[[ -n "$chat_sub" && "$chat_sub" != null ]] || fail "chat: whoami returned no subject"
chat_page="markdown/instances/conversation/c-verify-demo.md"
chat_group="page-chat:v1:$(printf %s "$chat_sub" | sha256sum | cut -d' ' -f1):$chat_page"
chat_doc="$(printf -- '---\nkind: instance\nskill: conversation\nid: c-verify-demo\ntitle: verify-demo\nowner: "%s"\nstarted_at: 2026-10-27T00:00:00Z\n---\n# verify-demo\n' "$chat_sub")"
chat_mcp update_page "$(jq -nc --arg p "$chat_page" --arg c "$chat_doc" '{page_id: $p, base_sha256: "", content: $c}')" \
  | jq -e '.result.structuredContent.ok == true' >/dev/null || fail "chat: could not create the conversation"
chat_mcp append_message "$(jq -nc --arg g "$chat_group" \
    '{chat_group_id: $g, role: "user", content: "Is the demo alive?", msg_id: "verify-1", embed: false}')" \
  | jq -e '.result.structuredContent.msg_id == "verify-1"' >/dev/null || fail "chat: user turn refused"
chat_mcp capture_event "$(jq -nc --arg g "$chat_group" --arg p "$chat_page" \
    '{label_skill: "chat", source: "verify-demo", instance_page_id: $p, title: "chat turn",
      provenance: {chat_group_id: $g, user_msg_id: "verify-1"}}')" \
  | jq -e '.result.structuredContent.event_id' >/dev/null || fail "chat: capture refused"
chat_answer=""
for _ in $(seq 1 120); do
  chat_answer="$(chat_mcp list_messages "$(jq -nc --arg g "$chat_group" '{chat_group_id: $g, direction: "asc"}')" \
    | jq -r '.result.structuredContent.messages[] | select(.role == "assistant") | .content' 2>/dev/null)"
  [[ -n "$chat_answer" ]] && break
  sleep 0.5
done
[[ "$chat_answer" == "echo: Is the demo alive?" ]] \
  || fail "chat: no assistant answer from the run (got '$chat_answer'; see $CHAT_HOME/runner.log)"
note "probe ok: chat — a question was answered by a run as the asker (logs: $CHAT_HOME)"
echo PASS
