#!/usr/bin/env bash
# Tests for scripts/image-tags.sh: publish-image.yml turns the pushed tag into docker tags and runs with
# `packages: write`, so a hostile tag name must be refused BEFORE it is used and a plain version must give the
# image tags and the gdrive requirement. Run: bash scripts/test-image-tags.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
T="$HERE/image-tags.sh"
SHA=0123456789abcdef0123456789abcdef01234567
IMG=ghcr.io/datazoode/escurel
pass=0; fail=0
ok()  { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1" >&2; }

run() { REF_TYPE="$1" REF_NAME="$2" GIT_SHA="$SHA" "$T" "$IMG" 2>&1; }

out="$(run tag v2026.10.17)" || bad "a plain tag was refused: $out"
grep -qx "tags=$IMG:v2026.10.17,$IMG:0123456789ab" <<<"$out" && ok "tag -> :tag + :sha12" || bad "tag tags wrong: $out"
grep -qx 'require_gdrive=1' <<<"$out" && ok "tag requires gdrive" || bad "tag must require gdrive: $out"

out="$(run tag v2026.10.17-rc1)" && grep -q "$IMG:v2026.10.17-rc1" <<<"$out" && ok "rc tag accepted" || bad "rc tag: $out"

out="$(run branch main)" || bad "branch refused: $out"
grep -qx "tags=$IMG:main,$IMG:0123456789ab" <<<"$out" && ok "branch -> :main + :sha12" || bad "branch tags wrong: $out"
grep -qx 'require_gdrive=0' <<<"$out" && ok "branch does not require gdrive" || bad "branch gdrive flag: $out"

for hostile in 'v1$(id)' 'v1;touch-x' 'v1"x' 'v1`id`' 'v1|x' 'v1&x' 'vfoo' 'v' 'v1 x'; do
  if out="$(run tag "$hostile")"; then bad "hostile tag '$hostile' was accepted: $out"; else ok "refused tag $hostile"; fi
  grep -q '^tags=' <<<"$out" && bad "hostile tag '$hostile' still printed tags" || true
done

echo "image-tags: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
