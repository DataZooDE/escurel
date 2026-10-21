#!/usr/bin/env bash
# Tests for scripts/release-semver.sh: Cargo AND the VSIX (npm/vsce) both take the tag's version and both reject
# what the tag scheme produces (`2026.08.02` has leading zeros; a same-day re-cut `2026.10.15.1` has four parts).
# Run: bash scripts/test-release-semver.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
S="$HERE/release-semver.sh"
pass=0; fail=0
ok()  { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1" >&2; }

is() { # input expected
  local out; out="$("$S" "$1" 2>&1)" && [ "$out" = "$2" ] && ok "$1 -> $2" || bad "$1: wanted $2, got: $out"
}

is 2026.10.15 2026.10.15
is 2026.08.02 2026.8.2
is 2026.08.02-rc1 2026.8.2-rc1
is 2026.10.15.1 2026.10.15+1
is 2026.10.15.12 2026.10.15+12
is 2026.08.02.3-rc1 2026.8.2-rc1+3
is 2026.10.15+meta 2026.10.15+meta
is 2026.10.15.1+meta 2026.10.15+1.meta
is 0.0.0-dev 0.0.0-dev

# The result is what npm and cargo accept: three numeric components, no leading zeros.
for v in 2026.08.02 2026.10.15.1 2026.08.02.3-rc1; do
  out="$("$S" "$v")"
  if printf '%s' "$out" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
    ok "$v is valid SemVer: $out"
  else
    bad "$v -> $out is not valid SemVer"
  fi
done

echo "release-semver: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
