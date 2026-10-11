#!/bin/sh
# One-shot, runs in a node container: lays out the demo's data under /demo (a volume the gateway and the
# demo services share). The demo seed names its data sources with placeholders; they are resolved here
# to the paths the gateway will see.
#
# One script serves both demo stacks; DEMO_INIT_MODE selects the variation:
#   plain (default)  the classic web-demo gateway WITHOUT a token verifier: pre-writes
#                    state/gateway.json and state/bearer.json for the story scripts.
#   s2d              the S2D stack (escurel-test-gateway, which VERIFIES tokens): the gateway
#                    writes its own state as it starts, so nothing is pre-written here; the layout
#                    is idempotent via a .laid-out marker (compose may run this again on a later
#                    `up`, and the data must not be wiped under a running gateway), and the query
#                    pages move under logistics/methods where the story is.
#
# DEMO_SRC / DEMO_DIR override the source and target dirs (defaults /demo-src and /demo); the compose
# stacks leave them unset. They exist so the layout can be exercised without docker (see test-init.sh).
set -eu
SRC=${DEMO_SRC:-/demo-src}
D=${DEMO_DIR:-/demo}
MODE=${DEMO_INIT_MODE:-plain}
case "$MODE" in
  plain|s2d) ;;
  *) echo "unknown DEMO_INIT_MODE: '$MODE' (want plain|s2d)" >&2; exit 1 ;;
esac
# Idempotent in s2d mode: compose may run this again on a later `up`; the data it laid out must not be
# wiped under a running gateway.
if [ "$MODE" = s2d ] && [ -f "$D/.laid-out" ]; then echo "demo data already laid out"; exit 0; fi
rm -rf "$D/seed" "$D/sources" "$D/sqlite" "$D/secrets" "$D/state"
cp -r "$SRC/seed" "$D/seed"
cp -r "$SRC/sources" "$D/sources"
sed -i "s|@ORDER_LINES_DIR@|$D/sources/order-lines|" "$D/seed/skills/order-lines.md"
sed -i "s|@VBAK_DIR@|$D/sources/vbak|"             "$D/seed/skills/customer-order.md"
sed -i "s|@LFA1_DIR@|$D/sources/lfa1|"             "$D/seed/skills/supplier.md"

if [ "$MODE" = s2d ]; then
  # With the S2D demo the query pages are the METHODS behind its numbers (the teaser opens one): they sit under
  # logistics, where the story is, not under plumbing (run.sh does the same for the desktop demo).
  sed -i 's|^folder: plumbing$|folder: logistics/methods|; s|^title: Query$|title: Methods|' "$D/seed/skills/query.md"
fi

# The SQL database behind `orders-db` and the secret FILE that references it (a tenant never names a path).
# The tenant of both compose stacks is `demo`: a tenant's secret files live under <secrets>/<tenant>/.
mkdir -p "$D/sqlite" "$D/secrets/demo" "$D/state"
node "$D/sources/orders-db/seed.mjs" "$D/sqlite/orders.db" 2>/dev/null
printf '%s\n' "$D/sqlite/orders.db" > "$D/secrets/demo/orders-db"

if [ "$MODE" = plain ]; then
  # What the story scripts read: the gateway address (and no bearer: this gateway runs without a verifier,
  # on the compose network only).
  printf '{"gateway_url":"http://escurel:8080","admin_bearer":"","tenant":"demo"}\n' > "$D/state/gateway.json"
  printf '{"bearer":""}\n' > "$D/state/bearer.json"
else
  touch "$D/.laid-out"
fi

# The gateway runs as uid 65532 and writes the SQLite file (a reviewed write-back). Skipped when not root
# (local/test runs); the compose stacks run this container as root, so they still chown.
if [ "$(id -u)" -eq 0 ]; then chown -R 65532:65532 "$D"; fi
echo "demo data laid out under $D"
