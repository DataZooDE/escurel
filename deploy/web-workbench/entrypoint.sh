#!/bin/sh
# Escurel web workbench: checks the password, renders the settings, starts code-server.
set -eu

fail() { echo "escurel-web: $*" >&2; exit 64; }

# --- the password: this container is a remote-code-execution surface behind one shared secret ------
# Either a plain PASSWORD or an argon2 HASHED_PASSWORD (preferred: the hash, not the secret, sits in
# the environment). Refuse to start without one, or with an obvious placeholder.
if [ -n "${HASHED_PASSWORD:-}" ]; then
  case "$HASHED_PASSWORD" in
    '$argon2'*) ;;
    *) fail "HASHED_PASSWORD is not an argon2 hash (generate one: echo -n 'secret' | npx argon2-cli -e)" ;;
  esac
elif [ -n "${PASSWORD:-}" ]; then
  [ "${#PASSWORD}" -ge 12 ] || fail "PASSWORD must be at least 12 characters"
  case "$(printf %s "$PASSWORD" | tr '[:upper:]' '[:lower:]')" in
    *change*me*|*changeme*|*password*|*escurel*|*secret*|*12345*) fail "PASSWORD is a placeholder; set a real one" ;;
  esac
else
  fail "set PASSWORD (at least 12 characters) or HASHED_PASSWORD (argon2): the workbench will not start unauthenticated"
fi

DATA="${WORKBENCH_DATA_DIR:-/home/coder/data}"
mkdir -p "$DATA" /home/coder/workspace

NODE=/usr/lib/code-server/lib/node
"$NODE" /opt/escurel/render-settings.mjs /opt/escurel/settings.base.json /opt/escurel/keybindings.json "$DATA"

set -- --auth password \
  --bind-addr "0.0.0.0:${PORT:-8080}" \
  --user-data-dir "$DATA" \
  --extensions-dir /opt/escurel/extensions \
  --config /opt/escurel/code-server.yaml \
  --disable-telemetry --disable-update-check --disable-workspace-trust \
  --disable-file-downloads --disable-file-uploads --disable-proxy \
  --disable-getting-started-override \
  --app-name Escurel

# TLS terminated here instead of by a proxy (optional): both files, or neither.
if [ -n "${CODE_SERVER_CERT:-}" ] || [ -n "${CODE_SERVER_CERT_KEY:-}" ]; then
  [ -n "${CODE_SERVER_CERT:-}" ] && [ -n "${CODE_SERVER_CERT_KEY:-}" ] || fail "set both CODE_SERVER_CERT and CODE_SERVER_CERT_KEY"
  set -- "$@" --cert "$CODE_SERVER_CERT" --cert-key "$CODE_SERVER_CERT_KEY"
fi
# Behind a reverse proxy on another origin, the browser's Origin header must be accepted.
[ -z "${CODE_SERVER_TRUSTED_ORIGINS:-}" ] || set -- "$@" --trusted-origins "$CODE_SERVER_TRUSTED_ORIGINS"
# Behind a proxy that serves the workbench under a path prefix.
[ -z "${CODE_SERVER_ABS_PROXY_BASE_PATH:-}" ] || set -- "$@" --abs-proxy-base-path "$CODE_SERVER_ABS_PROXY_BASE_PATH"

exec dumb-init /usr/bin/code-server "$@" /home/coder/workspace
