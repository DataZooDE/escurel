#!/bin/sh
# One-shot: registers the demo's outside systems and plays the story into the gateway (a supplier-risk
# signal that is promoted, and a second one left open for review).
#
# One script serves both demo stacks; DEMO_STORY_MODE selects the variation:
#   plain (default)  the classic web-demo gateway WITHOUT a token verifier: plays the story straight
#                    into the pre-written gateway state. Re-running it on the same data duplicates the
#                    story (`docker compose down -v` starts clean).
#   s2d              the S2D stack (escurel-test-gateway, which VERIFIES tokens): waits for the gateway
#                    and the runner, refreshes the live bearer tokens, plays the story and loads
#                    the S2D demo on top, and reports what waits for the planner. Idempotent via a
#                    story.done marker: a fresh gateway starts empty, so compose replays this whenever the
#                    gateway is new.
#
# DEMO_SRC / DEMO_DIR override the source and target dirs (defaults /demo-src and /demo); the compose
# stacks leave them unset. They exist so the story can be exercised without docker (see test-story.sh).
# DEMO_STORY_DRY_RUN=1 prints the exact commands the story WOULD run (node invocations with their args
# and environment, marker file handling) instead of running them.
set -eu
SRC=${DEMO_SRC:-/demo-src}
D=${DEMO_DIR:-/demo}
S=$D/state
MODE=${DEMO_STORY_MODE:-plain}
case "$MODE" in
  plain|s2d) ;;
  *) echo "unknown DEMO_STORY_MODE: '$MODE' (want plain|s2d)" >&2; exit 1 ;;
esac
DRY=${DEMO_STORY_DRY_RUN:-0}
# run executes its arguments, or just prints them (+ prefix) as a dry run.
run() {
  if [ "$DRY" = 1 ]; then printf '+ %s\n' "$*"; else "$@"; fi
}
# putenv exports NAME=VALUE, and prints it too as a dry run (exporting is side-effect free).
putenv() {
  if [ "$DRY" = 1 ]; then printf '+ export %s=%s\n' "$1" "$2"; fi
  export "$1=$2"
}
# out runs a command with its stdout redirected to a file (printed whole as a dry run).
out() {
  dest=$1; shift
  if [ "$DRY" = 1 ]; then printf '+ %s > %s\n' "$*" "$dest"; else "$@" > "$dest"; fi
}
# spin waits until COND (a shell condition string) holds, printing it as a dry run (where the state
# files it waits on may not exist yet, so waiting would never end).
spin() {
  if [ "$DRY" = 1 ]; then printf '+ until %s; do sleep 1; done\n' "$1"; else eval "$1"; fi
}
run cd "$SRC"
if [ "$MODE" = s2d ] && [ -f "$S/story.done" ]; then echo "story already played"; exit 0; fi
if [ "$MODE" = s2d ]; then
  # The gateway, the runner and the services come up in their own time (the machine may be busy).
  spin "[ -s \"$S/gateway.json\" ] && [ -s \"$S/bearer.json\" ]"
  port="$(sed -n 's/.*"gateway_url":"http:\/\/127.0.0.1:\([0-9]*\)".*/\1/p' "$S/gateway.json" 2>/dev/null | head -1)"
  spin "node -e \"fetch('http://127.0.0.1:$port/healthz').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))\""
  spin "node -e \"fetch('http://127.0.0.1:8088/healthz').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))\""
  # The tokens the gateway printed at start expire within minutes; the bearer file is kept fresh. The scripts take their
  # bearers from the gateway file, so build a copy that carries the current ones.
  run node -e "
const fs = require('fs');
const g = JSON.parse(fs.readFileSync('$S/gateway.json', 'utf8').split('\n')[0]);
const b = JSON.parse(fs.readFileSync('$S/bearer.json', 'utf8'));
g.bearer = b.bearer; g.admin_bearer = b.admin_bearer;
fs.writeFileSync('$S/live.json', JSON.stringify(g) + '\n');
"
  G=$S/live.json
  putenv ESCUREL_DEMO_WAIT_MS 900000
else
  G=$D/state/gateway.json
fi
putenv ESCUREL_DEMO_RATINGS_URL http://127.0.0.1:9101
putenv ESCUREL_DEMO_CONFIRMATIONS_URL http://127.0.0.1:9102/mcp
putenv ESCUREL_DEMO_ORDERS_DB_SECRET "$D/secrets/demo/orders-db"
run node materialise.mjs "$G"
out "$S/story.json" node driver.mjs "$G" "$S/bearer.json"
if [ "$MODE" = s2d ]; then
  if [ -n "${S2D_DIR:-}" ] && [ -d "$S2D_DIR/pages" ]; then
    echo "loading the S2D demo (hetzner seed $(cat "$S2D_DIR/STAMP"))..."
    out "$S/s2d-story.json" node s2d/seed.mjs "$G" "$S/bearer.json" "$S2D_DIR"
  fi
  # What waits for the planner right now (the stage starts with the three S2D proposals open, nothing promoted).
  if [ "$DRY" = 1 ]; then
    printf '+ %s\n' "node s2d/check-awaiting.mjs $G $S/bearer.json | tee $S/awaiting.txt"
  else
    node s2d/check-awaiting.mjs "$G" "$S/bearer.json" | tee "$S/awaiting.txt"
  fi
  run touch "$S/story.done"
  echo "story played"
else
  run cat "$S/story.json"
fi
