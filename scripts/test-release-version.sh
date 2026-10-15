#!/usr/bin/env bash
# Tests for scripts/release-version.sh: the version taken from a tag name reaches shell `run:` blocks in
# release.yml, so a tag name with shell metacharacters must be refused. Run: bash scripts/test-release-version.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
V="$HERE/release-version.sh"
pass=0; fail=0
ok()  { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1" >&2; }

accepts() { # ref expected-version
  local out; out="$("$V" "$1" 2>&1)" && [ "$out" = "$2" ] && ok "$1 -> $2" || bad "$1: wanted $2, got: $out"
}
refuses() { # ref
  local out; if out="$("$V" "$1" 2>&1)"; then bad "$1 was accepted as '$out'"; else ok "refused $1"; fi
}

accepts refs/tags/v2026.10.15 2026.10.15
accepts refs/tags/v2026.10.15-rc1 2026.10.15-rc1
accepts refs/tags/v2026.10.15.2 2026.10.15.2
accepts refs/tags/v2026.10.15+2 2026.10.15+2
accepts refs/heads/main 0.0.0-dev
accepts refs/heads/feature/x 0.0.0-dev
refuses 'refs/tags/v1$(id)'
refuses 'refs/tags/v1;touch-x'
refuses 'refs/tags/v1"x'
refuses 'refs/tags/v1`id`'
refuses 'refs/tags/v1|x'
refuses 'refs/tags/v1&x'
refuses 'refs/tags/vfoo'
refuses 'refs/tags/v'

echo "release-version: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
