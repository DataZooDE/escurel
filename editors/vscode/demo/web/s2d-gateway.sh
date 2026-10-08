#!/bin/sh
# The S2D stack's gateway: escurel-test-gateway (escurel-server + a built-in token issuer), because the
# demo needs a gateway that tells WHICH RUN wrote what. It listens on a random loopback port; the
# forwarder (forward.mjs, same network namespace) makes it reachable as escurel:8080.
#
# State it writes: /demo/state/gateway.json (the line the gateway prints: it holds the issuer's signing key,
# readers are the demo's own scripts, never the workbench), /demo/state/signing.pem (for the runner), the bearer
# files, and /bearer-pub/bearer.json = a USER bearer only (no admin bearer, no key), which is all the workbench's
# extension host reads.
set -eu
S=/demo/state
mkdir -p "$S"
rm -f "$S/gateway.json" "$S/bearer.json" "$S/signing.pem" /bearer-pub/bearer.json
/usr/local/bin/escurel-test-gateway --tenant demo --seed /demo/seed --subject alice \
  --bearer-file "$S/bearer.json" > "$S/gateway.json" &
gw=$!
# Once the gateway has printed its line: the runner's key as a PEM file, and the public bearer, kept fresh.
(
  while [ ! -s "$S/gateway.json" ]; do sleep 0.5; done
  sed -n 's/.*"signing_key":"\([^"]*\)".*/\1/p' "$S/gateway.json" | head -1 | { read -r raw; printf '%b\n' "$raw" > "$S/signing.pem"; }
  while kill -0 "$gw" 2>/dev/null; do
    b="$(sed -n 's/.*"bearer":"\([^"]*\)".*/\1/p' "$S/bearer.json" 2>/dev/null | head -1)"
    if [ -n "$b" ]; then printf '{"bearer":"%s"}\n' "$b" > /bearer-pub/bearer.tmp && mv /bearer-pub/bearer.tmp /bearer-pub/bearer.json; fi
    sleep 20
  done
) &
trap 'kill "$gw" 2>/dev/null || true' TERM INT
wait "$gw"
