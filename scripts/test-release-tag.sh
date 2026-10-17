#!/usr/bin/env bash
# Tests for scripts/release-tag.sh: `git tag -a -m/-F` strips every line starting with '#', so the markdown
# headings of a CHANGELOG section (`## vX — BREAKING`, `### BREAKING`) vanished from the v2026.10.17 tag
# message and release.yml refused it. The script must keep them. Run: bash scripts/test-release-tag.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
T="$HERE/release-tag.sh"
pass=0; fail=0
ok()  { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1" >&2; }

repo="$(mktemp -d)"; trap 'rm -rf "$repo"' EXIT
cd "$repo" && git init -q . && git config user.email t@t && git config user.name t
cat > CHANGELOG.md <<'MD'
## Unreleased

## v2026.10.17 — BREAKING (stored format)

Read first: the runbook.

### BREAKING
- the `kind:` cut

## v2026.07.13
- older
MD
git add . && git commit -qm c

# the trap itself, so the test documents why the script exists
git tag -a plain -m "$(sed -n '/^## v2026.10.17/,/^## /p' CHANGELOG.md | sed '$d')"
n="$(git for-each-ref refs/tags/plain --format='%(contents)' | grep -c BREAKING || true)"
[ "$n" = 0 ] && ok "plain git tag -m strips the BREAKING headings" || bad "plain git tag kept $n BREAKING lines (git changed?)"

if "$T" v2026.10.17 >/dev/null 2>&1; then
  n="$(git for-each-ref refs/tags/v2026.10.17 --format='%(contents)' | grep -c BREAKING || true)"
  [ "$n" = 2 ] && ok "release-tag.sh keeps both BREAKING headings" || bad "release-tag.sh: $n BREAKING lines, wanted 2"
  [ "$(git cat-file -t refs/tags/v2026.10.17)" = tag ] && ok "annotated tag" || bad "not an annotated tag"
  git for-each-ref refs/tags/v2026.10.17 --format='%(contents)' | grep -q '^## v2026.10.17' && ok "heading line preserved" || bad "heading stripped"
  git for-each-ref refs/tags/v2026.10.17 --format='%(contents)' | grep -q 'older' && bad "leaked the next section" || ok "next section excluded"
else
  bad "release-tag.sh v2026.10.17 failed or is missing"
fi

"$T" v2099.01.01 >/dev/null 2>&1 && bad "tag without a CHANGELOG section accepted" || ok "refused a tag with no CHANGELOG section"
git tag -l v2099.01.01 | grep -q . && bad "left a tag behind on refusal" || ok "no tag left on refusal"

echo "$pass passed, $fail failed"; [ "$fail" = 0 ]
