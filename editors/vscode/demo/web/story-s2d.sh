#!/bin/sh
# One-shot (S2D stack): registers the demo's outside systems, plays the story into the gateway and loads the
# S2D demo on top. It shares the gateway's network namespace. Idempotent: a marker file stops a restart from
# playing it twice (`docker compose down -v` starts clean).
set -eu
D=/demo
S=$D/state
if [ -f "$S/story.done" ]; then echo "story already played"; exit 0; fi
cd /demo-src
# The gateway, the runner and the services come up in their own time (the machine may be busy).
until [ -s "$S/gateway.json" ] && [ -s "$S/bearer.json" ]; do sleep 1; done
port="$(sed -n 's/.*"gateway_url":"http:\/\/127.0.0.1:\([0-9]*\)".*/\1/p' "$S/gateway.json" | head -1)"
until node -e "fetch('http://127.0.0.1:$port/healthz').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"; do sleep 1; done
until node -e "fetch('http://127.0.0.1:8088/healthz').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"; do sleep 1; done
export ESCUREL_DEMO_WAIT_MS=900000
export ESCUREL_DEMO_RATINGS_URL=http://127.0.0.1:9101
export ESCUREL_DEMO_CONFIRMATIONS_URL=http://127.0.0.1:9102/mcp
export ESCUREL_DEMO_ORDERS_DB_SECRET="$D/secrets/demo/orders-db"
node materialise.mjs "$S/gateway.json"
node driver.mjs "$S/gateway.json" "$S/bearer.json" > "$S/story.json"
if [ -n "${S2D_DIR:-}" ] && [ -d "$S2D_DIR/pages" ]; then
  echo "loading the S2D demo (hetzner seed $(cat "$S2D_DIR/STAMP"))..."
  node s2d/seed.mjs "$S/gateway.json" "$S/bearer.json" "$S2D_DIR" > "$S/s2d-story.json"
fi
touch "$S/story.done"
echo "story played"
