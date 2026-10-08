#!/bin/sh
# One-shot (S2D stack): lays out the demo's data under /demo like init.sh, for a gateway that VERIFIES
# tokens (escurel-test-gateway). What differs from init.sh: the gateway writes its own state
# (state/gateway.json, the bearer files) as it starts, so nothing is pre-written here.
set -eu
SRC=/demo-src
D=/demo
# Idempotent: compose may run this again on a later `up`; the data it laid out must not be wiped under a running gateway.
if [ -f "$D/.laid-out" ]; then echo "demo data already laid out"; exit 0; fi
rm -rf "$D/seed" "$D/sources" "$D/sqlite" "$D/secrets" "$D/state"
cp -r "$SRC/seed" "$D/seed"
cp -r "$SRC/sources" "$D/sources"
sed -i "s|@ORDER_LINES_DIR@|$D/sources/order-lines|" "$D/seed/skills/order-lines.md"
sed -i "s|@VBAK_DIR@|$D/sources/vbak|"             "$D/seed/skills/customer-order.md"
sed -i "s|@LFA1_DIR@|$D/sources/lfa1|"             "$D/seed/skills/supplier.md"

# With the S2D demo the query pages are the METHODS behind its numbers (the teaser opens one): they sit under
# logistics, where the story is, not under plumbing (run.sh does the same for the desktop demo).
sed -i 's|^folder: plumbing$|folder: logistics/methods|; s|^title: Query$|title: Methods|' "$D/seed/skills/query.md"

# The tenant of this stack is `demo`: a tenant's secret files live under <secrets>/<tenant>/.
mkdir -p "$D/sqlite" "$D/secrets/demo" "$D/state"
node "$D/sources/orders-db/seed.mjs" "$D/sqlite/orders.db" 2>/dev/null
printf '%s\n' "$D/sqlite/orders.db" > "$D/secrets/demo/orders-db"

touch "$D/.laid-out"
chown -R 65532:65532 "$D"
echo "demo data laid out under $D"
