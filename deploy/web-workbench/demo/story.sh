#!/bin/sh
# One-shot: registers the demo's outside systems and plays the story into the gateway (a supplier-risk
# signal that is promoted, and a second one left open for review). Idempotent enough to re-run after
# `docker compose down -v`; running it twice on the same data duplicates the story.
set -eu
D=/demo
cd /demo-src
export ESCUREL_DEMO_RATINGS_URL=http://127.0.0.1:9101
export ESCUREL_DEMO_CONFIRMATIONS_URL=http://127.0.0.1:9102/mcp
export ESCUREL_DEMO_ORDERS_DB_SECRET="$D/secrets/demo/orders-db"
node materialise.mjs "$D/state/gateway.json"
node driver.mjs "$D/state/gateway.json" "$D/state/bearer.json" > "$D/state/story.json"
cat "$D/state/story.json"
