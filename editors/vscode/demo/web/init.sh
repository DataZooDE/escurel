#!/bin/sh
# One-shot, runs in a node container: lays out the demo's data under /demo (a volume the gateway and the
# demo services share). The demo seed names its data sources with placeholders; they are resolved here
# to the paths the gateway will see.
set -eu
SRC=/demo-src
D=/demo
rm -rf "$D/seed" "$D/sources" "$D/sqlite" "$D/secrets" "$D/state"
cp -r "$SRC/seed" "$D/seed"
cp -r "$SRC/sources" "$D/sources"
sed -i "s|@ORDER_LINES_DIR@|$D/sources/order-lines|" "$D/seed/skills/order-lines.md"
sed -i "s|@VBAK_DIR@|$D/sources/vbak|"             "$D/seed/skills/customer-order.md"
sed -i "s|@LFA1_DIR@|$D/sources/lfa1|"             "$D/seed/skills/supplier.md"

# The SQL database behind `orders-db` and the secret FILE that references it (a tenant never names a path).
# The tenant of this compose is `demo`: a tenant's secret files live under <secrets>/<tenant>/.
mkdir -p "$D/sqlite" "$D/secrets/demo" "$D/state"
node "$D/sources/orders-db/seed.mjs" "$D/sqlite/orders.db" 2>/dev/null
printf '%s\n' "$D/sqlite/orders.db" > "$D/secrets/demo/orders-db"

# What the story scripts read: the gateway address (and no bearer: this gateway runs without a verifier,
# on the compose network only).
printf '{"gateway_url":"http://escurel:8080","admin_bearer":"","tenant":"demo"}\n' > "$D/state/gateway.json"
printf '{"bearer":""}\n' > "$D/state/bearer.json"

# The gateway runs as uid 65532 and writes the SQLite file (a reviewed write-back).
chown -R 65532:65532 "$D"
echo "demo data laid out under $D"
