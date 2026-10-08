#!/bin/sh
# One-shot (S2D stack): lays out the demo's data under /demo like init.sh, for a gateway that VERIFIES
# tokens (escurel-test-gateway). What differs from init.sh: the gateway writes its own state
# (state/gateway.json, the bearer files) as it starts, so nothing is pre-written here, and the one
# volume the workbench may read (`/bearer-pub`, a user bearer ONLY, never the admin bearer or the key) is created.
set -eu
SRC=/demo-src
D=/demo
rm -rf "$D/seed" "$D/sources" "$D/sqlite" "$D/secrets" "$D/state"
cp -r "$SRC/seed" "$D/seed"
cp -r "$SRC/sources" "$D/sources"
sed -i "s|@ORDER_LINES_DIR@|$D/sources/order-lines|" "$D/seed/skills/order-lines.md"
sed -i "s|@VBAK_DIR@|$D/sources/vbak|"             "$D/seed/skills/customer-order.md"
sed -i "s|@LFA1_DIR@|$D/sources/lfa1|"             "$D/seed/skills/supplier.md"

# The tenant of this stack is `demo`: a tenant's secret files live under <secrets>/<tenant>/.
mkdir -p "$D/sqlite" "$D/secrets/demo" "$D/state"
node "$D/sources/orders-db/seed.mjs" "$D/sqlite/orders.db" 2>/dev/null
printf '%s\n' "$D/sqlite/orders.db" > "$D/secrets/demo/orders-db"

mkdir -p /bearer-pub
chown -R 65532:65532 "$D" /bearer-pub
echo "demo data laid out under $D"
