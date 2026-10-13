#!/usr/bin/env bash
# Tests for scripts/release-gate.sh without a tag, a network or a real GitHub API: the gate's decision
# logic runs against JSON fixtures (--dry-run). Run: bash scripts/test-release-gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$HERE/release-gate.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
pass=0; fail=0
ok()  { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1" >&2; }

SHA=0123456789abcdef0123456789abcdef01234567
JOB_OK='{"name":"fmt + clippy + test","conclusion":"success"}'
JOB_RED='{"name":"fmt + clippy + test","conclusion":"failure"}'
RUN_OK='{"databaseId":2,"status":"completed","conclusion":"success","createdAt":"2026-10-13T10:00:00Z"}'
RUN_RED='{"databaseId":2,"status":"completed","conclusion":"failure","createdAt":"2026-10-13T10:00:00Z"}'
RUN_BUSY='{"databaseId":2,"status":"in_progress","conclusion":"","createdAt":"2026-10-13T10:00:00Z"}'

fixture() { # name ancestor polls-json
  printf '{"ancestor":%s,"polls":%s}\n' "$2" "$3" > "$TMP/$1.json"
}
# expect NAME EXPECTED_EXIT GREP_PATTERN [ENV...]: run the gate on fixture NAME.
expect() {
  local name=$1 want=$2 pat=$3; shift 3
  local out rc
  out="$(env RELEASE_GATE_TIMEOUT_SECS=2 RELEASE_GATE_POLL_SECS=1 "$@" "$GATE" "$SHA" --dry-run "$TMP/$name.json" 2>&1)"; rc=$?
  if [ "$rc" = "$want" ] && grep -q -E "$pat" <<<"$out"; then ok "$name -> exit $want"; else
    bad "$name: wanted exit $want and /$pat/, got exit $rc: $out"; fi
}

fixture green true "[{\"runs\":[$RUN_OK],\"jobs\":[$JOB_OK]}]"
fixture red-run true "[{\"runs\":[$RUN_RED],\"jobs\":[$JOB_RED]}]"
fixture red-job true "[{\"runs\":[$RUN_OK],\"jobs\":[$JOB_RED]}]"
fixture no-job true "[{\"runs\":[$RUN_OK],\"jobs\":[{\"name\":\"build --release\",\"conclusion\":\"success\"}]}]"
fixture busy true "[{\"runs\":[$RUN_BUSY],\"jobs\":[]}]"
fixture missing true "[{\"runs\":[],\"jobs\":[]}]"
fixture busy-then-green true "[{\"runs\":[$RUN_BUSY],\"jobs\":[]},{\"runs\":[$RUN_OK],\"jobs\":[$JOB_OK]}]"
fixture not-ancestor false "[{\"runs\":[$RUN_OK],\"jobs\":[$JOB_OK]}]"
OLD_RED='{"databaseId":1,"status":"completed","conclusion":"failure","createdAt":"2026-10-13T09:00:00Z"}'
fixture rerun-wins true "[{\"runs\":[$OLD_RED,$RUN_OK],\"jobs\":[$JOB_OK]}]"
NEW_RED='{"databaseId":3,"status":"completed","conclusion":"failure","createdAt":"2026-10-13T11:00:00Z"}'
fixture newer-red-wins true "[{\"runs\":[$RUN_OK,$NEW_RED],\"jobs\":[$JOB_RED]}]"

expect green 0 "green"
expect red-run 1 "ended failure"
expect red-job 1 "ended failure"
expect no-job 1 "not found"
expect busy 1 "timed out"
expect missing 1 "timed out"
expect busy-then-green 0 "green"
expect not-ancestor 1 "not an ancestor"
expect rerun-wins 0 "green"
expect newer-red-wins 1 "ended failure"
# The escape hatch passes a red fixture, loudly.
expect red-run 0 "RELEASE_SKIP_CI_GATE" RELEASE_SKIP_CI_GATE=1
expect not-ancestor 0 "RELEASE_SKIP_CI_GATE" RELEASE_SKIP_CI_GATE=1
# Only the exact value 1 skips.
expect red-run 1 "ended failure" RELEASE_SKIP_CI_GATE=true

# Usage errors.
if "$GATE" >/dev/null 2>&1; then bad "no commit must be a usage error"; else
  [ "$?" = 2 ] && ok "no commit -> exit 2" || bad "no commit: wrong exit code"; fi
if "$GATE" not-a-sha --dry-run "$TMP/green.json" >/dev/null 2>&1; then bad "a non-sha must be refused"; else ok "a non-sha is refused"; fi

echo "pass=$pass fail=$fail"
[ "$fail" = 0 ]
