#!/bin/sh
# The S2D stack's gateway: escurel-test-gateway (escurel-server + a built-in token issuer), because the
# demo needs a gateway that tells WHICH RUN wrote what. It listens on a random loopback port; the
# forwarder (forward.mjs, same network namespace) makes it reachable as escurel:8080.
#
# State it writes: /demo/state/gateway.json (the FIRST line the gateway prints: it holds the issuer's signing key and the
# admin bearer; only the demo's own scripts read it, never the workbench), /demo/state/signing.pem (for the runner) and
# the bearer file the gateway keeps fresh. The workbench gets none of them: the forwarder signs its calls in.
set -eu
S=/demo/state
mkdir -p "$S"
# A fresh gateway starts EMPTY (its data lives in this container, not a volume): the story must be played again.
rm -f "$S/gateway.json" "$S/bearer.json" "$S/signing.pem" "$S/story.done" "$S/gateway.pid" "$S/live.json"
# stdout = the one JSON line first, then log lines: only the first line goes to the file (it would otherwise grow without
# bound and every reader would re-read it); the rest goes to the container's log.
{
  /usr/local/bin/escurel-test-gateway --tenant demo --seed /demo/seed --subject alice \
    --bearer-file "$S/bearer.json" &
  echo $! > "$S/gateway.pid"
  wait
} | { IFS= read -r line; printf '%s\n' "$line" > "$S/gateway.json"; cat >&2; } &
sh_pid=$!
# Once the gateway has printed its line: the runner's key as a PEM file.
(
  while [ ! -s "$S/gateway.json" ]; do sleep 0.5; done
  sed -n 's/.*"signing_key":"\([^"]*\)".*/\1/p' "$S/gateway.json" | head -1 | { read -r raw; printf '%b\n' "$raw" > "$S/signing.pem"; }
) &
trap 'kill "$(cat "$S/gateway.pid" 2>/dev/null)" 2>/dev/null || true' TERM INT
wait "$sh_pid"
