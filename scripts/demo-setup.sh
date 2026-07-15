#!/usr/bin/env bash
# Materialise the crm-demo's external-backend content against a RUNNING
# escurel-server (boot one first, e.g. via scripts/verify-demo.sh or
# `ESCUREL_SEED_DIR=examples/crm-demo cargo run -p escurel-server`).
#
# ESCUREL_SEED_DIR only seeds *markdown pages* — external instances are
# backend-managed and must be materialised through the admin tools. This
# script drives exactly that, idempotently (safe to re-run):
#
#   1. sql_view — re-point the seeded `erp_order` skill's repo-relative
#      `relation:` at the ABSOLUTE sources/erp dir (DuckDB resolves the
#      glob against the server cwd; resolving here makes the demo
#      cwd-independent), then `create_sql_instance` the `book` instance.
#
# The server is expected to run without a verifier (the demo default), so
# the admin tools are open. Point ESCUREL_DEMO_BASE elsewhere to target a
# non-default port.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE="${ESCUREL_DEMO_BASE:-http://127.0.0.1:8080}"
MCP="$BASE/mcp"

fail() { echo "FAIL: $*" >&2; exit 1; }
note() { echo ">>> $*"; }

command -v curl >/dev/null || fail "curl not on PATH"
command -v jq >/dev/null || fail "jq not on PATH"

# POST one tools/call; prints the JSON-RPC reply. Fails the script on a
# transport error or a JSON-RPC error object.
mcp() {
  local tool="$1" args="$2" reply
  reply=$(jq -cn --arg t "$tool" --argjson a "$args" \
      '{jsonrpc:"2.0",id:1,method:"tools/call",params:{name:$t,arguments:$a}}' \
    | curl -fsS "$MCP" -H 'content-type: application/json' -d @-) \
    || fail "$tool: transport error against $MCP"
  if jq -e '.error' >/dev/null 2>&1 <<<"$reply"; then
    fail "$tool: $(jq -c '.error' <<<"$reply")"
  fi
  echo "$reply"
}

# --- 1. sql_view: erp_order over sources/erp ---------------------------

ERP_SKILL="$ROOT/examples/crm-demo/skills/erp_order.md"
ERP_DIR="$ROOT/examples/crm-demo/sources/erp"
[[ -f "$ERP_SKILL" ]] || fail "missing $ERP_SKILL"
[[ -d "$ERP_DIR" ]] || fail "missing $ERP_DIR"

note "erp_order: resolving relation to $ERP_DIR"
ERP_MD="$(sed "s|relation: examples/crm-demo/sources/erp|relation: $ERP_DIR|" "$ERP_SKILL")"
ERP_ARGS=$(jq -cn --arg c "$ERP_MD" '{page_id:"markdown/skills/erp_order.md",content:$c}')
UPD=$(mcp update_page "$ERP_ARGS")
jq -e '.result.structuredContent.ok == true' >/dev/null <<<"$UPD" \
  || fail "erp_order skill update rejected: $UPD"

note "erp_order: materialising instance [[erp_order::book]]"
CREATED=$(mcp create_sql_instance '{"skill":"erp_order","id":"book","overlay_body":"# ERP order book\nRead-only mirror of the ERP order extract shipped with the demo."}')
note "erp_order: $(jq -c '.result.structuredContent' <<<"$CREATED")"

note "demo setup complete"
