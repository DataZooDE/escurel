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
#      seed + as_of time-travel + scenario overlay + the event/inbox
#      tools resolve end-to-end, and
#   3. runs the *real* escurel-demo-agent for one pass to prove the full
#      M7 loop: capture → inbox → agent fold → instance event history.
#      Captures a screenshot.
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
# Metrics scraping is irrelevant to the demo and would otherwise bind
# the default :9090 — empty disables the dedicated listener so a
# co-running gateway (or a parallel demo run) doesn't hit a port clash.
ESCUREL_SEED_DIR="$SEED_DIR" \
ESCUREL_SERVE_DEMO_DIR="$APP/build/web" \
ESCUREL_SERVER_DATA_DIR="$DATA_DIR" \
ESCUREL_SERVER_LISTEN_HTTP="127.0.0.1:$PORT" \
ESCUREL_OBSERVABILITY_METRICS_LISTEN="" \
ESCUREL_EMBEDDING_PROVIDER="zero" \
ESCUREL_WEBHOOK_URL="$BASE/__webhook_sink" \
  cargo run -q -p escurel-server >"$ROOT/target/escurel-demo.log" 2>&1 &
SERVER_PID=$!
cleanup() { "$RODNEY" stop >/dev/null 2>&1 || true; kill "$SERVER_PID" >/dev/null 2>&1 || true; rm -rf "$DATA_DIR"; }
trap cleanup EXIT

note "waiting for /healthz"
for _ in $(seq 1 120); do curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "$BASE/healthz" >/dev/null 2>&1 || fail "server did not come up"

mkdir -p "$SHOTS"
"$RODNEY" start >/dev/null 2>&1 || fail "rodney start"
note "open $BASE/  (the editor — inbox/events/webhooks/members folded in)"
"$RODNEY" open "$BASE/" >/dev/null 2>&1 || fail "rodney open"

# --- presence: assert each CRM region's container semantics label ---

# Count flt-semantics nodes whose aria-label CONTAINS the token. A
# substring match (not exact) because Flutter merges a Tab's
# Semantics(label:) with its visible text into one aria-label value
# (e.g. `tab-events\nEvents`); the `tab-*` tokens are distinct so there's
# no cross-match. Container labels materialise reliably; nested
# excludeSemantics chips do not (see SCOPE).
label_count() {
  "$RODNEY" js "document.querySelectorAll('flt-semantics[aria-label*=\"$1\"]').length" 2>/dev/null
}
wait_label() {
  for _ in $(seq 1 30); do
    [[ "$(label_count "$1")" =~ ^[1-9] ]] && return 0
    "$RODNEY" sleep 1 >/dev/null 2>&1
  done
  return 1
}
present() { wait_label "$1" || fail "region semantics not found: $1"; note "present: $1"; }

# The editor (/) now folds the former CRM workspace's surfaces into a
# tabbed right panel — Links / Events (history + inbox) / Webhooks
# (outbound delivery log) / Members (group ACL) — plus a pinned capture
# bar. Assert the always-rendered container chrome: the brand + the four
# tab buttons. The tab CONTENT panes (event-pane, inbox,
# webhook-deliveries-pane, group-members-pane) only enter the DOM when
# their tab is active (TabBarView is lazy) and capture is a TextField that
# doesn't reliably materialise — all covered by the flutter widget tests +
# the /mcp behaviour probes below.
present tab-events
present tab-webhooks
present tab-members

# --- behaviour: drive the real backend via same-origin /mcp probes ---

# Call a tool and print one value the caller asks for. `extract` is a JS
# expression over the tool's structured payload (bound as `r`). A
# `tools/call` reply is an MCP `CallToolResult`
# (`{content, isError, structuredContent}`); the tool's own JSON is in
# `structuredContent`, so `r` is bound there (falling back to the raw
# result for any non-wrapped reply).
mcp_int() {
  local tool="$1" args="$2" extract="$3"
  "$RODNEY" js "(async()=>{const resp=await fetch('/mcp',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'$tool',arguments:$args}})});const j=await resp.json();if(!j.result)return 'ERR';const r=j.result.structuredContent||j.result;return String($extract)})()" 2>/dev/null
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

# (The recast crm-demo corpus carries emails/meetings/docs as *events*,
# not instances — the old per-skill instance-feed time-travel + doc-overlay
# probes are superseded by the event/inbox + expand(as_of) probes below.)

note "probe: resolve picks the scenario-B overlay page"
spine_base=$(mcp_int resolve '{"wikilink":"[[engagement::hoffmann-spine]]"}' "(r.page&&r.page.page_id||'').split('/').pop()")
spine_b=$(mcp_int resolve '{"wikilink":"[[engagement::hoffmann-spine]]","scenario":"B"}' "(r.page&&r.page.page_id||'').split('/').pop()")
[[ "$spine_base" != "$spine_b" && -n "$spine_b" ]] || fail "scenario B did not override resolve ($spine_base vs $spine_b)"
note "probe ok: resolve base=$spine_base, scenario B=$spine_b"

note "probe: event history bound to the engagement spine"
spine_id=$(mcp_int resolve '{"wikilink":"[[engagement::hoffmann-spine]]"}' "(r.page&&r.page.page_id||'')")
[[ -n "$spine_id" && "$spine_id" != "ERR" ]] || fail "could not resolve spine page id (got '$spine_id')"
spine_events=$(mcp_int list_events "{\"instance_page_id\":\"$spine_id\"}" 'r.events.length')
[[ "$spine_events" =~ ^[1-9] ]] || fail "no events bound to the spine (got '$spine_events')"
note "probe ok: spine events = $spine_events"

# --- enriched corpus: graph + grouping (links footer, menus) ---------
note "probe: spine neighbours (backlinks + outgoing) for the links footer"
backlinks=$(mcp_int neighbours "{\"page_id\":\"$spine_id\",\"direction\":\"in\"}" 'r.edges.length')
outgoing=$(mcp_int neighbours "{\"page_id\":\"$spine_id\",\"direction\":\"out\"}" 'r.edges.length')
[[ "$backlinks" -ge 7 ]] || fail "spine should have ≥7 backlinks (got '$backlinks')"
[[ "$outgoing" -ge 5 ]] || fail "spine should have ≥5 outgoing links (got '$outgoing')"
note "probe ok: spine backlinks=$backlinks, outgoing=$outgoing"

note "probe: skills registry has event-typed + entity-bound skills"
sk_event=$(mcp_int list_skills '{}' 'r.skills.filter(function(s){return s.is_event_typed}).length')
sk_entity=$(mcp_int list_skills '{}' 'r.skills.filter(function(s){return !s.is_event_typed}).length')
[[ "$sk_event" -ge 1 && "$sk_entity" -ge 2 ]] \
  || fail "expected event-typed + entity-bound skills (event=$sk_event entity=$sk_entity)"
note "probe ok: skills event-typed=$sk_event, entity-bound=$sk_entity"

# --- external instance backends: wire surface + document ingestion -----
# The `attachment` skill (examples/crm-demo/skills/attachment.md) declares a
# `document` backend, so list_skills must carry the additive backend +
# capabilities wire (PR-1b) and report it read-only. (The sql_view backend
# needs an attached external DB + a registered credential — out of scope for
# the air-gapped demo; it's covered by the no-mock Rust e2e
# sql_view_backend.rs / fusion_acl.rs. Noted here so the gap is explicit.)
note "probe: list_skills carries the backend + capabilities wire (PR-1b)"
attach="r.skills.find(function(s){return s.id==='attachment'})"
bk_doc=$(mcp_int list_skills '{}' "(($attach)&&($attach).backend&&($attach).backend.kind)||''")
expect_int "attachment backend kind" "$bk_doc" "document"
caps_ro=$(mcp_int list_skills '{}' "String((($attach)&&($attach).capabilities&&($attach).capabilities.writable))")
expect_int "attachment is read-only" "$caps_ro" "false"

# POST a born-digital text blob through the real /ingest/upload endpoint
# (same origin as the served bundle, no auth in the demo) and read the
# evented pipeline's structured outcome.
ingest_text() {
  local ct="$1" b64="$2" title="$3" extract="$4"
  "$RODNEY" js "(async()=>{const resp=await fetch('/ingest/upload',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({content_type:'$ct',bytes_b64:'$b64',title:'$title'})});const r=await resp.json();return String($extract)})()" 2>/dev/null
}
note "probe: /ingest/upload extracts → chunks → materialises a document instance"
DOC_B64=$(printf 'Acme master services agreement.\n\nClause 1: term is 24 months.\n\nClause 2: net-30 payment terms apply.' | base64 | tr -d '\n')
ING=$(ingest_text "text/plain" "$DOC_B64" "msa probe" "[r.status,r.handler_skill,r.chunk_count,r.page_id].join('~')")
IFS='~' read -r ing_status ing_handler ing_chunks ing_page <<< "$ING"
[[ "$ing_status" == "materialised" ]] || fail "ingest did not materialise (got '$ing_status' from '$ING')"
[[ "$ing_handler" == "attachment" ]] || fail "ingest routed to wrong skill (got '$ing_handler')"
[[ "$ing_chunks" =~ ^[1-9] ]] || fail "ingest produced no chunks (got '$ing_chunks')"
note "probe ok: ingest → status=$ing_status handler=$ing_handler chunks=$ing_chunks page=$ing_page"

note "probe: the ingested page is a read-only document instance (backend_ref)"
ing_kind=$(mcp_int expand "{\"page_id\":\"$ing_page\"}" "(r.frontmatter&&r.frontmatter.backend_ref&&r.frontmatter.backend_ref.kind)||''")
expect_int "ingested page backend kind" "$ing_kind" "document"

note "probe: an unmatched MIME parks with no_handler_skill (blob retained)"
park=$(ingest_text "application/x-binary" "$DOC_B64" "junk" "(r.issue&&r.issue.code)||r.status")
expect_int "unmatched mime parked" "$park" "no_handler_skill"

# The default server build ships the kreuzberg extractor (PDF/DOCX/PPTX/XLSX),
# and the `attachment` skill accepts application/pdf — so a real born-digital
# PDF must extract → chunk → materialise through the same /ingest/upload path,
# with no feature flag. Proves the document backend is usable out of the box.
note "probe: /ingest/upload extracts a real PDF (kreuzberg, default build)"
PDF_FIXTURE="$ROOT/crates/escurel-server/tests/fixtures/report.pdf"
[[ -f "$PDF_FIXTURE" ]] || fail "PDF fixture missing: $PDF_FIXTURE"
PDF_B64=$(base64 < "$PDF_FIXTURE" | tr -d '\n')
PDFING=$(ingest_text "application/pdf" "$PDF_B64" "report.pdf" "[r.status,r.handler_skill,r.chunk_count].join('~')")
IFS='~' read -r pdf_status pdf_handler pdf_chunks <<< "$PDFING"
[[ "$pdf_status" == "materialised" ]] || fail "PDF ingest did not materialise (got '$pdf_status' from '$PDFING')"
[[ "$pdf_handler" == "attachment" ]] || fail "PDF ingest routed to wrong skill (got '$pdf_handler')"
[[ "$pdf_chunks" =~ ^[1-9] ]] || fail "PDF ingest produced no chunks (got '$pdf_chunks')"
note "probe ok: PDF ingest → status=$pdf_status handler=$pdf_handler chunks=$pdf_chunks"

# --- group ACL v1: admin membership tools round-trip through the gateway -
# The demo server runs without a verifier (dev/on-host), so admin tools
# are open here — this proves add_group_member → list_group_members works
# end-to-end through the served bundle's same-origin /mcp. (The auth-gated
# path is covered by the no-mock Rust e2e group_members_acl.rs.)
note "probe: add_group_member then list_group_members round-trips"
add_ok=$(mcp_int add_group_member '{"group_id":"team-acme","subject":"whatsapp:probe"}' "(r.ok?'ok':'no')")
expect_int "add_group_member" "$add_ok" "ok"
gm_has=$(mcp_int list_group_members '{"group_id":"team-acme"}' "r.members.some(function(m){return m.subject==='whatsapp:probe'})")
expect_int "list_group_members contains seeded subject" "$gm_has" "true"

note "probe: Instances directory is multi-account (per-skill counts)"
n_customer=$(mcp_int list_instances '{"skill_id":"customer"}' 'r.instances.length')
n_workstream=$(mcp_int list_instances '{"skill_id":"workstream"}' 'r.instances.length')
[[ "$n_customer" -ge 3 ]] || fail "expected ≥3 customers (got '$n_customer')"
[[ "$n_workstream" -ge 4 ]] || fail "expected ≥4 workstreams (got '$n_workstream')"
note "probe ok: customers=$n_customer, workstreams=$n_workstream"

note "probe: capture_event appends to the inbox"
inbox_before=$(mcp_int list_inbox '{}' 'r.events.length')
[[ "$inbox_before" =~ ^[0-9] ]] || fail "list_inbox failed (got '$inbox_before')"
cap_ok=$(mcp_int capture_event '{"source":"manual","mime":"text/plain","label_skill":"note","title":"e2e probe","body":"e2e probe"}' "(r.status||'')")
[[ "$cap_ok" == "inbox" ]] || fail "capture_event did not return an inbox event (got '$cap_ok')"
inbox_after=$(mcp_int list_inbox '{}' 'r.events.length')
[[ "$inbox_after" -gt "$inbox_before" ]] || fail "inbox did not grow after capture ($inbox_after !> $inbox_before)"
note "probe ok: inbox $inbox_before → $inbox_after after capture"

# --- outbound webhook delivery log (the new "webhook callbacks" view) ---
# ESCUREL_WEBHOOK_URL is set above, so each capture fires a POST whose
# outcome is recorded. Prove the admin_webhook_deliveries tool surfaces it.
note "probe: webhook delivery log records the capture callbacks"
wh_cfg=$(mcp_int admin_webhook_deliveries '{}' "(r.configured?'yes':'no')")
expect_int "webhook configured" "$wh_cfg" "yes"
wh_n=""
for _ in $(seq 1 40); do
  wh_n=$(mcp_int admin_webhook_deliveries '{}' 'r.deliveries.length')
  [[ "$wh_n" =~ ^[1-9] ]] && break
  "$RODNEY" sleep 1 >/dev/null 2>&1
done
[[ "$wh_n" =~ ^[1-9] ]] || fail "no outbound webhook deliveries recorded (got '$wh_n')"
note "probe ok: webhook deliveries logged = $wh_n"

note "probe: expand(as_of) re-materialises the spine's state at T"
name_now=$(mcp_int expand "{\"page_id\":\"$spine_id\"}" "(r.frontmatter&&r.frontmatter.phase||'')")
name_early=$(mcp_int expand "{\"page_id\":\"$spine_id\",\"as_of\":\"2026-03-13T00:00:00Z\"}" "(r.frontmatter&&r.frontmatter.phase||'')")
[[ -n "$name_now" && -n "$name_early" && "$name_now" != "$name_early" ]] \
  || fail "expand(as_of) did not show a different historical phase (now='$name_now', early='$name_early')"
note "probe ok: spine phase now='$name_now', as_of(T)='$name_early'"

note "probe: list_snapshots exposes the spine's state-over-time markers"
snap_count=$(mcp_int list_snapshots "{\"page_id\":\"$spine_id\"}" 'r.snapshots.length')
[[ "$snap_count" -ge 2 ]] || fail "spine has too few snapshots for version nav (got '$snap_count')"
note "probe ok: spine snapshots = $snap_count"

# --- live loop: capture → inbox → escurel-demo-agent → instance ------
# The full M7 vision end-to-end: capture an event pre-flagged for the
# spine (so it lands in the inbox), run the *real* external agent for a
# single pass against this gateway, and assert it folded the event into
# the spine's history (out of the inbox, into list_events).
note "probe: live capture → agent fold loop"
events_before=$(mcp_int list_events "{\"instance_page_id\":\"$spine_id\"}" 'r.events.length')
inbox_pre=$(mcp_int list_inbox '{}' 'r.events.length')
cap_live=$(mcp_int capture_event \
  "{\"source\":\"gmail\",\"mime\":\"message/rfc822\",\"label_skill\":\"gmail\",\"instance_page_id\":\"$spine_id\",\"title\":\"live-loop probe\",\"body\":\"live-loop probe\"}" \
  "(r.status||'')")
[[ "$cap_live" == "inbox" ]] || fail "live capture did not land in the inbox (got '$cap_live')"
inbox_mid=$(mcp_int list_inbox '{}' 'r.events.length')
[[ "$inbox_mid" -gt "$inbox_pre" ]] || fail "inbox did not grow after live capture ($inbox_mid !> $inbox_pre)"

note "building escurel-demo-agent"
cargo build -q -p escurel-demo-agent || fail "cargo build escurel-demo-agent"
AGENT_BIN="$ROOT/target/debug/escurel-demo-agent"
[[ -x "$AGENT_BIN" ]] || fail "agent binary not found at $AGENT_BIN"

note "running one agent pass (ESCUREL_AGENT_ONCE=1)"
ESCUREL_AGENT_MCP_URL="$BASE/mcp" \
ESCUREL_AGENT_TOKEN="demo" \
ESCUREL_AGENT_ONCE="1" \
  "$AGENT_BIN" >"$ROOT/target/escurel-demo-agent.log" 2>&1 \
  || fail "agent single pass failed (see target/escurel-demo-agent.log)"

events_after=$(mcp_int list_events "{\"instance_page_id\":\"$spine_id\"}" 'r.events.length')
inbox_post=$(mcp_int list_inbox '{}' 'r.events.length')
[[ "$events_after" -gt "$events_before" ]] \
  || fail "agent did not fold the event into the spine ($events_after !> $events_before)"
[[ "$inbox_post" -lt "$inbox_mid" ]] \
  || fail "agent did not drain the captured event from the inbox ($inbox_post !< $inbox_mid)"
note "probe ok: live loop — spine events $events_before → $events_after, inbox $inbox_mid → $inbox_post"

"$RODNEY" screenshot "$SHOTS/crm.png" >/dev/null 2>&1 || true

note "ALL REGIONS PRESENT + BACKEND PROBES PASSED — screenshot in $SHOTS/crm.png"
echo "PASS"
