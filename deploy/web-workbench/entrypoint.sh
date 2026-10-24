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
  # Default minimum 12; an operator may lower it (never below 8) with MIN_PASSWORD_LENGTH.
  min="${MIN_PASSWORD_LENGTH:-12}"
  case "$min" in ''|*[!0-9]*) fail "MIN_PASSWORD_LENGTH must be a number" ;; esac
  [ "$min" -ge 8 ] || fail "MIN_PASSWORD_LENGTH may not be below 8"
  [ "${#PASSWORD}" -ge "$min" ] || fail "PASSWORD must be at least $min characters"
  case "$(printf %s "$PASSWORD" | tr '[:upper:]' '[:lower:]')" in
    *change*me*|*changeme*|*password*|*escurel*|*secret*|*12345*) fail "PASSWORD is a placeholder; set a real one" ;;
  esac
else
  fail "set PASSWORD (at least 12 characters) or HASHED_PASSWORD (argon2): the workbench will not start unauthenticated"
fi

# --- the downstream overlay (OVERLAY.md): validated again here, so a bad layer fails loudly at start -----
OPT="${WORKBENCH_OPT:-/opt/escurel}"
OVERLAY="${WORKBENCH_OVERLAY_DIR:-$OPT/overlay}"
NODE="${WORKBENCH_NODE:-/usr/lib/code-server/lib/node}"
"$NODE" "$OPT/overlay.mjs" check "$OVERLAY" || exit $?
APP_NAME="$("$NODE" "$OPT/overlay.mjs" get productName "$OVERLAY")"
WELCOME="$("$NODE" "$OPT/overlay.mjs" get welcomeText "$OVERLAY")"

DATA="${WORKBENCH_DATA_DIR:-/home/coder/data}"
mkdir -p "$DATA" /home/coder/workspace

"$NODE" "$OPT/render-settings.mjs" "$OPT/settings.common.json" "$OPT/settings.base.json" "$OPT/keybindings.json" "$DATA" "$OVERLAY"

set -- --auth password \
  --bind-addr "0.0.0.0:${PORT:-8080}" \
  --user-data-dir "$DATA" \
  --extensions-dir "$OPT/extensions" \
  --config "$OPT/code-server.yaml" \
  --disable-telemetry --disable-update-check --disable-workspace-trust \
  --disable-file-downloads --disable-file-uploads --disable-proxy \
  --disable-getting-started-override \
  --app-name "${APP_NAME:-Escurel}" --welcome-text "${WORKBENCH_WELCOME_TEXT:-${WELCOME:-Sign in to ${APP_NAME:-Escurel}}}"

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
