#!/usr/bin/env bash
# Smoke test of the Escurel web workbench image: builds it, boots it hardened, and checks over HTTP that
#   - it refuses to start without a real password,
#   - /healthz answers, the root redirects to /login, a wrong password is refused, the right one logs in,
#   - the node-pty module is gone (no terminal can ever spawn) and the marketplace gallery is off.
# With --browser it also drives a real headless Chromium through deploy/web-workbench/probe/probe.mjs
# (needs editors/vscode/node_modules for playwright-core, and a Chromium: CHROMIUM=/usr/bin/chromium).
#
#   scripts/web-workbench-smoke.sh [--browser] [--no-build]
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${WORKBENCH_IMAGE:-escurel-web-smoke/workbench:dev}"
NAME="escurel-web-smoke-$$"
PORT="${WORKBENCH_SMOKE_PORT:-$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')}"
PASSWORD_OK="Zq7-kind-Walrus-91-smoke"
BROWSER=0; BUILD=1
for a in "$@"; do case "$a" in --browser) BROWSER=1 ;; --no-build) BUILD=0 ;; *) echo "unknown flag $a" >&2; exit 2 ;; esac; done

pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*" >&2; exit 1; }
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

if [ "$BUILD" = 1 ]; then
  DOCKER_BUILDKIT=1 docker build -q -f "$REPO/deploy/web-workbench/Dockerfile" -t "$IMAGE" "$REPO" >/dev/null
fi

# 1. It refuses to start without a real password (and says why).
for env in "" "PASSWORD=change-me-before-first-start" "PASSWORD=short" "HASHED_PASSWORD=not-a-hash"; do
  if out=$(docker run --rm ${env:+-e "$env"} -e WORKBENCH_GATEWAY_URL=http://gw:8080 "$IMAGE" 2>&1); then
    fail "started with [$env]"
  fi
  echo "$out" | grep -q 'escurel-web:' || fail "no explanation for [$env]: $out"
done
pass "refuses an empty, placeholder, short or malformed password"

# 2. Boot it hardened: read-only rootfs, no capabilities, no new privileges.
docker run -d --name "$NAME" --read-only --cap-drop ALL --security-opt no-new-privileges:true \
  --tmpfs /tmp --tmpfs /home/coder/data:uid=1000,gid=1000 --tmpfs /home/coder/.config:uid=1000,gid=1000 \
  --tmpfs /home/coder/workspace:uid=1000,gid=1000 \
  -e PASSWORD="$PASSWORD_OK" -e WORKBENCH_GATEWAY_URL=http://gw:8080 -p "127.0.0.1:$PORT:8080" "$IMAGE" >/dev/null
for _ in $(seq 1 60); do curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null || { docker logs "$NAME" >&2; fail "/healthz never answered"; }
pass "/healthz answers with a read-only root filesystem and no capabilities"

# 3. Login.
code=$(curl -s -o /dev/null -w '%{http_code} %{redirect_url}' "http://127.0.0.1:$PORT/")
[ "$code" = "302 http://127.0.0.1:$PORT/login" ] || fail "root did not redirect to /login: $code"
bad=$(curl -s -X POST -d 'password=wrong-wrong-wrong' "http://127.0.0.1:$PORT/login")
echo "$bad" | grep -qi 'incorrect' || fail "a wrong password was not refused"
good=$(curl -s -o /dev/null -D - -X POST -d "password=$PASSWORD_OK" "http://127.0.0.1:$PORT/login")
echo "$good" | grep -qi '^set-cookie: code-server-session' || fail "the right password did not log in"
pass "unauthenticated requests redirect to /login; a wrong password is refused; the right one logs in"

# 4. The lock-down that does not depend on a setting.
docker exec "$NAME" sh -c 'test ! -e /usr/lib/code-server/lib/vscode/node_modules/node-pty/build/Release/pty.node' \
  || fail "node-pty's native module is still present (a terminal could spawn)"
docker exec "$NAME" sh -c 'test ! -e /usr/bin/sudo' || fail "sudo is still installed"
docker exec "$NAME" sh -c 'test "$(id -u)" != 0' || fail "running as root"
docker exec "$NAME" sh -c 'test "$EXTENSIONS_GALLERY" = "{}"' || fail "the marketplace gallery is not disabled"
pass "no pty module, no sudo, non-root, marketplace gallery off"

# 5. A real browser (optional).
if [ "$BROWSER" = 1 ]; then
  (cd "$REPO" && node deploy/web-workbench/probe/probe.mjs "http://127.0.0.1:$PORT" "$PASSWORD_OK" "${WORKBENCH_SMOKE_OUT:-/tmp/escurel-web-smoke}") || fail "browser probe"
  pass "browser probe"
fi
echo "web workbench smoke: OK"
