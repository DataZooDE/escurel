#!/bin/sh
# Equivalence / behaviour test for the unified demo init script (../init.sh).
#
# Runs the init script in both modes against fresh temp dirs (no docker, no root:
# DEMO_SRC/DEMO_DIR override the container paths) and asserts the per-mode
# contract that the two old scripts (init.sh = plain, init-s2d.sh = s2d)
# implemented. Also proves s2d idempotency and that the default mode is plain.
#
#   sh editors/vscode/demo/web/test-init.sh
#
# INIT_UNDER_TEST overrides the script under test (default: the sibling
# init.sh); used to show the test goes red against a broken variant.
set -eu
HERE=$(dirname "$0")
INIT=${INIT_UNDER_TEST:-$HERE/init.sh}
SRC=$(cd "$HERE/.." && pwd)

pass=0; fail=0
ok() { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1"; }
is_eq() { [ "$2" = "$3" ] && ok "$1" || bad "$1 (want [$3], got [$2])"; }

command -v node >/dev/null || { echo "FAIL: node not on PATH"; exit 1; }
sh -n "$INIT" || { echo "FAIL: sh -n rejects $INIT"; exit 1; }
ok "sh -n accepts $INIT"

# --- plain mode (the old init.sh): pre-writes gateway state, no marker, query page untouched.
P=$(mktemp -d); trap 'rm -rf "$P" "$S" "$Q"' EXIT
DEMO_SRC="$SRC" DEMO_DIR="$P" DEMO_INIT_MODE=plain sh "$INIT" >/dev/null
is_eq "plain gateway.json" '{"gateway_url":"http://escurel:8080","admin_bearer":"","tenant":"demo"}' "$(cat "$P/state/gateway.json")"
is_eq "plain bearer.json" '{"bearer":""}' "$(cat "$P/state/bearer.json")"
[ ! -e "$P/.laid-out" ] && ok "plain writes no .laid-out marker" || bad "plain writes no .laid-out marker"
grep -q '^folder: plumbing$' "$P/seed/skills/query.md" && grep -q '^title: Query$' "$P/seed/skills/query.md" \
  && ok "plain leaves query.md under plumbing" || bad "plain leaves query.md under plumbing"
if grep -rq '@ORDER_LINES_DIR@\|@VBAK_DIR@\|@LFA1_DIR@' "$P/seed"; then bad "plain resolves seed placeholders"; else ok "plain resolves seed placeholders"; fi
grep -q "$P/sources/order-lines" "$P/seed/skills/order-lines.md" \
  && ok "plain resolves placeholder to target dir" || bad "plain resolves placeholder to target dir"
is_eq "plain secret pointer" "$P/sqlite/orders.db" "$(cat "$P/secrets/demo/orders-db")"
[ "$(node -e "const {DatabaseSync}=require('node:sqlite');const db=new DatabaseSync('$P/sqlite/orders.db');console.log(db.prepare('SELECT COUNT(*) v FROM orders').get().v);db.close();")" = "5" ] \
  && ok "plain seeds orders.db (5 rows)" || bad "plain seeds orders.db (5 rows)"

# --- default mode is plain: same tree as explicit plain (modulo the target dir
# baked into the resolved placeholders and the secret pointer).
Q=$(mktemp -d)
DEMO_SRC="$SRC" DEMO_DIR="$Q" sh "$INIT" >/dev/null
grep -rlI "$P" "$P" | xargs sed -i "s|$P|@D|g"
grep -rlI "$Q" "$Q" | xargs sed -i "s|$Q|@D|g"
diff -r -x orders.db "$P" "$Q" >/dev/null && ok "default mode matches explicit plain" || bad "default mode matches explicit plain"
cmp -s "$P/sqlite/orders.db" "$Q/sqlite/orders.db" && ok "default mode seeds identical orders.db" || bad "default mode seeds identical orders.db"

# --- s2d mode (the old init-s2d.sh): no pre-written state, marker, query page moved.
S=$(mktemp -d)
DEMO_SRC="$SRC" DEMO_DIR="$S" DEMO_INIT_MODE=s2d sh "$INIT" >/dev/null
[ ! -e "$S/state/gateway.json" ] && [ ! -e "$S/state/bearer.json" ] \
  && ok "s2d pre-writes no gateway state" || bad "s2d pre-writes no gateway state"
[ -f "$S/.laid-out" ] && ok "s2d writes .laid-out marker" || bad "s2d writes .laid-out marker"
grep -q '^folder: logistics/methods$' "$S/seed/skills/query.md" && grep -q '^title: Methods$' "$S/seed/skills/query.md" \
  && ok "s2d moves query.md to logistics/methods" || bad "s2d moves query.md to logistics/methods"
if grep -rq '@ORDER_LINES_DIR@\|@VBAK_DIR@\|@LFA1_DIR@' "$S/seed"; then bad "s2d resolves seed placeholders"; else ok "s2d resolves seed placeholders"; fi
is_eq "s2d secret pointer" "$S/sqlite/orders.db" "$(cat "$S/secrets/demo/orders-db")"

# --- s2d idempotency: a second run changes nothing.
echo sentinel > "$S/state/sentinel"
sum_before=$(sha256sum "$S/sqlite/orders.db" | cut -d' ' -f1)
out=$(DEMO_SRC="$SRC" DEMO_DIR="$S" DEMO_INIT_MODE=s2d sh "$INIT")
is_eq "s2d second run says already laid out" "demo data already laid out" "$out"
[ "$(cat "$S/state/sentinel")" = "sentinel" ] && ok "s2d second run wipes nothing" || bad "s2d second run wipes nothing"
is_eq "s2d second run keeps orders.db" "$sum_before" "$(sha256sum "$S/sqlite/orders.db" | cut -d' ' -f1)"

# --- unknown mode is rejected.
if DEMO_SRC="$SRC" DEMO_DIR="$S" DEMO_INIT_MODE=bogus sh "$INIT" 2>/dev/null; then bad "unknown mode is rejected"; else ok "unknown mode is rejected"; fi

echo "---"
echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
