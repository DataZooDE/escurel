#!/usr/bin/env bash
# Presence + behavioural verification for the escurel "data zoo / CRM"
# demo (apps/escurel-explore at /#/crm), using rodney (a Go
# Chrome-automation CLI) plus in-page MCP probes.
#
# Builds the Flutter web bundle in HTTP mode, boots escurel-server
# serving it at / with the crm-demo corpus seeded, then:
#   1. asserts (exit-code-gated) that each CRM region's semantics label
#      exists in the accessibility tree, and
#   2. drives the *real backend* via in-page POST /mcp probes — proving
#      seed + frontmatter filter + as_of time-travel + scenario overlay
#      all resolve end-to-end — and captures a screenshot.
#
# SCOPE — read this before extending:
#   Flutter web renders to a CanvasKit <canvas>. Its semantics tree is
#   exposed as DOM (flt-semantics[aria-label=…]), so rodney can *read*
#   it and screenshot it. But Flutter dispatches gestures from pointer
#   events hit-tested against its glasspane, NOT from DOM events on the
#   semantics nodes — so a DOM/CDP click does not fire button callbacks,
#   and deeply-nested excludeSemantics nodes (wheel-node, inbox-item)
#   don't reliably materialise in the static DOM. So this script asserts
#   *container* region labels for presence and uses in-page /mcp fetches
#   (same origin as the served bundle) for behaviour. Click-through
#   coverage lives in the flutter widget tests + the no-mock Rust
#   integration tests. Do NOT add ax/JS clicks expecting them to drive
#   Flutter — they won't.
#
# Exit 0 = all region labels present + all backend probes passed.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/apps/escurel-explore"
SEED_DIR="${ESCUREL_DEMO_SEED:-$ROOT/examples/crm-demo}"
PORT="${ESCUREL_DEMO_PORT:-8080}"
BASE="http://127.0.0.1:$PORT"
SHOTS="${ESCUREL_DEMO_SHOTS:-$ROOT/target/demo-shots}"
RODNEY="${RODNEY:-rodney}"

fail() { echo "FAIL: $*" >&2; exit 1; }
note() { echo ">>> $*"; }

command -v "$RODNEY" >/dev/null || fail "rodney not on PATH"
command -v flutter >/dev/null || fail "flutter not on PATH"
command -v curl >/dev/null || fail "curl not on PATH"

# The served bundle must run in HTTP mode so it talks to the gateway's
# own /mcp (same origin); plain `flutter build web` stays in standalone
# fixture mode and would never hit the seeded backend.
if [[ "${BUILD:-1}" == "1" || ! -f "$APP/build/web/index.html" ]]; then
  note "flutter build web (HTTP mode)"
  (cd "$APP" && flutter build web --no-tree-shake-icons \
      --dart-define=ESCUREL_EXPLORE_MODE=http >/dev/null) \
    || fail "flutter build web"
fi

DATA_DIR="$(mktemp -d)"
note "starting escurel-server (seed=$SEED_DIR, data=$DATA_DIR)"
ESCUREL_SEED_DIR="$SEED_DIR" \
ESCUREL_SERVE_DEMO_DIR="$APP/build/web" \
ESCUREL_SERVER_DATA_DIR="$DATA_DIR" \
ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:$PORT" \
ESCUREL_SERVER_LISTEN_GRPC="" \
ESCUREL_EMBEDDING_PROVIDER="zero" \
  cargo run -q -p escurel-server >"$ROOT/target/escurel-demo.log" 2>&1 &
SERVER_PID=$!
cleanup() { "$RODNEY" stop >/dev/null 2>&1 || true; kill "$SERVER_PID" >/dev/null 2>&1 || true; rm -rf "$DATA_DIR"; }
trap cleanup EXIT

note "waiting for /healthz"
for _ in $(seq 1 120); do curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "$BASE/healthz" >/dev/null 2>&1 || fail "server did not come up"

mkdir -p "$SHOTS"
"$RODNEY" start >/dev/null 2>&1 || fail "rodney start"
note "open $BASE/#/crm"
"$RODNEY" open "$BASE/#/crm" >/dev/null 2>&1 || fail "rodney open"

# --- presence: assert each CRM region's container semantics label ---

# Count flt-semantics nodes carrying an exact aria-label. Container
# labels (region-*, skill-wheel, …) materialise reliably; nested
# excludeSemantics chips do not (see SCOPE).
label_count() {
  "$RODNEY" js "document.querySelectorAll('flt-semantics[aria-label=\"$1\"]').length" 2>/dev/null
}
wait_label() {
  for _ in $(seq 1 30); do
    [[ "$(label_count "$1")" =~ ^[1-9] ]] && return 0
    "$RODNEY" sleep 1 >/dev/null 2>&1
  done
  return 1
}
present() { wait_label "$1" || fail "region semantics not found: $1"; note "present: $1"; }

# Wait for the workspace to boot + auto-focus, then assert the regions.
present brand
present region-inbox
present region-reader
present skill-wheel
present time-scrubber
present scenario-switch

# --- behaviour: drive the real backend via same-origin /mcp probes ---

# Call a tool and print one integer the caller asks for. `extract` is a
# JS expression over the JSON-RPC `result` object (bound as `r`).
mcp_int() {
  local tool="$1" args="$2" extract="$3"
  "$RODNEY" js "(async()=>{const resp=await fetch('/mcp',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'$tool',arguments:$args}})});const j=await resp.json();if(!j.result)return 'ERR';const r=j.result;return String($extract)})()" 2>/dev/null
}
expect_int() {
  local label="$1" got="$2" want="$3"
  [[ "$got" == "$want" ]] || fail "$label: expected $want, got '$got'"
  note "probe ok: $label = $got"
}

note "probe: seed populated (engagement instances)"
eng=$(mcp_int list_instances '{"skill_id":"engagement"}' 'r.instances.length')
[[ "$eng" =~ ^[1-9] ]] || fail "no engagement instances seeded (got '$eng')"
note "probe ok: engagement instances = $eng"

note "probe: as_of time-travel (email inbox shrinks before the corpus)"
email_now=$(mcp_int list_instances '{"skill_id":"email","order_by":"at desc"}' 'r.instances.length')
email_early=$(mcp_int list_instances '{"skill_id":"email","order_by":"at desc","as_of":"2026-03-13T00:00:00Z"}' 'r.instances.length')
[[ "$email_now" =~ ^[1-9] ]] || fail "no emails seeded (got '$email_now')"
[[ "$email_early" -lt "$email_now" ]] || fail "as_of did not shrink the email feed ($email_early !< $email_now)"
note "probe ok: emails now=$email_now, as_of(T+1d)=$email_early"

note "probe: scenario overlay (doc inbox grows under B)"
doc_base=$(mcp_int list_instances '{"skill_id":"doc","order_by":"at desc"}' 'r.instances.length')
doc_b=$(mcp_int list_instances '{"skill_id":"doc","order_by":"at desc","scenario":"B"}' 'r.instances.length')
[[ "$doc_b" -gt "$doc_base" ]] || fail "scenario B did not add a doc ($doc_b !> $doc_base)"
note "probe ok: docs base=$doc_base, scenario B=$doc_b"

note "probe: resolve picks the scenario-B overlay page"
spine_base=$(mcp_int resolve '{"wikilink":"[[engagement::hoffmann-spine]]"}' "(r.page&&r.page.page_id||'').split('/').pop()")
spine_b=$(mcp_int resolve '{"wikilink":"[[engagement::hoffmann-spine]]","scenario":"B"}' "(r.page&&r.page.page_id||'').split('/').pop()")
[[ "$spine_base" != "$spine_b" && -n "$spine_b" ]] || fail "scenario B did not override resolve ($spine_base vs $spine_b)"
note "probe ok: resolve base=$spine_base, scenario B=$spine_b"

"$RODNEY" screenshot "$SHOTS/crm.png" >/dev/null 2>&1 || true

note "ALL REGIONS PRESENT + BACKEND PROBES PASSED — screenshot in $SHOTS/crm.png"
echo "PASS"
