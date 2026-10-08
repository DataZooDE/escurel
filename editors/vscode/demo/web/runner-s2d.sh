#!/bin/sh
# The runner of the S2D stack, MINTED mode (it signs a token per run, which is how the gateway tells which
# run wrote what, so the thread shows a changeset under its run). It shares the gateway's network namespace
# and takes the gateway's address and the issuer's key from the state the gateway wrote.
set -eu
S=/demo/state
until [ -s "$S/gateway.json" ] && [ -s "$S/signing.pem" ]; do sleep 0.5; done
field() { sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p" "$S/gateway.json" | head -1; }
export ESCUREL_RUNNER_GATEWAY_URL="$(field gateway_url)"
export ESCUREL_RUNNER_TENANT=demo
export ESCUREL_RUNNER_AUTH_ISSUER="$(field issuer_url)"
export ESCUREL_RUNNER_AUTH_KID="$(field kid)"
export ESCUREL_RUNNER_AUTH_SIGNING_KEY="$(cat "$S/signing.pem")"
export ESCUREL_RUNNER_HARNESS=echo
export ESCUREL_RUNNER_LISTEN=127.0.0.1:8088
export ESCUREL_RUNNER_POLL_INTERVAL=250ms
export ESCUREL_RUNNER_LEDGER_PATH=/data/ledger.duckdb
# The demo gateway is NEW every time it starts (its data lives in its container), so a ledger and cursors persisted
# from an earlier gateway describe events that no longer exist and would make the runner skip the replayed story.
rm -f /data/ledger.duckdb /data/ledger.duckdb.wal
unset ESCUREL_RUNNER_TOKEN
exec /usr/local/bin/escurel-runner
