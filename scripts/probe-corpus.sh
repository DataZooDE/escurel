#!/usr/bin/env bash
# Headless no-mock check of the enriched corpus over the real gateway
# (no rodney/Chrome — just /mcp probes). Boots escurel-server with the
# crm-demo seed on $PORT, asserts the new graph + grouping, tears down.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${PORT:-8090}"
BASE="http://127.0.0.1:$PORT"
fail() { echo "FAIL: $*" >&2; exit 1; }

DATA="$(mktemp -d)"
ESCUREL_SEED_DIR="$ROOT/examples/crm-demo" \
ESCUREL_SERVER_DATA_DIR="$DATA" \
ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:$PORT" \
ESCUREL_EMBEDDING_PROVIDER="zero" \
  "$ROOT/target/debug/escurel-server" >"$ROOT/target/probe-gw.log" 2>&1 &
PID=$!
trap 'kill "$PID" 2>/dev/null; rm -rf "$DATA"' EXIT

for _ in $(seq 1 90); do curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "$BASE/healthz" >/dev/null 2>&1 || fail "server did not come up"
sleep 2  # let seeding settle

mcp() { # tool, args-json, python-extract over result
  local body; body=$(curl -fsS -X POST "$BASE/mcp" -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" 2>/dev/null)
  echo "$body" | python3 -c "import sys,json;r=json.load(sys.stdin).get('result');print($3)" 2>/dev/null
}

spine=$(mcp resolve '{"wikilink":"[[engagement::hoffmann-spine]]"}' "r['page']['page_id']")
[[ -n "$spine" ]] || fail "resolve spine"
echo "spine=$spine"

bl=$(mcp neighbours "{\"page_id\":\"$spine\",\"direction\":\"in\"}" "len(r['edges'])")
og=$(mcp neighbours "{\"page_id\":\"$spine\",\"direction\":\"out\"}" "len(r['edges'])")
[[ "$bl" -ge 7 ]] || fail "spine backlinks <7 (got $bl)"
[[ "$og" -ge 5 ]] || fail "spine outgoing <5 (got $og)"
echo "OK backlinks=$bl outgoing=$og"

ev=$(mcp list_skills '{}' "sum(1 for s in r['skills'] if s['is_event_typed'])")
en=$(mcp list_skills '{}' "sum(1 for s in r['skills'] if not s['is_event_typed'])")
[[ "$ev" -ge 1 && "$en" -ge 2 ]] || fail "skills grouping (event=$ev entity=$en)"
echo "OK skills event=$ev entity=$en"

cu=$(mcp list_instances '{"skill_id":"customer"}' "len(r['instances'])")
ws=$(mcp list_instances '{"skill_id":"workstream"}' "len(r['instances'])")
ib=$(mcp list_inbox '{}' "len(r['events'])")
[[ "$cu" -ge 3 ]] || fail "customers <3 (got $cu)"
[[ "$ws" -ge 4 ]] || fail "workstreams <4 (got $ws)"
[[ "$ib" -ge 5 ]] || fail "inbox <5 (got $ib)"
echo "OK customers=$cu workstreams=$ws inbox=$ib"
echo "PASS"
