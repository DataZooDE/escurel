#!/usr/bin/env bash
# Every one-shot Job script (scripts/*-job.sh) must ship in the server image's runtime stage as
# /usr/local/bin/<name without .sh>, executable, and the image build must assert it. A Job in the lab runs
# `--entrypoint events-copy-job` / `migrate-kind-job` from the image; a script that is only in the repository is
# found out when the window is already open (the v2026.10.18 image shipped migrate-kind-job but not events-copy-job).
# Cheap: it reads the Dockerfile and the git file modes, it builds nothing. Run: bash scripts/test-image-scripts.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
DOCKERFILE="${DOCKERFILE:-$REPO/Dockerfile}"
pass=0; fail=0
ok()  { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1" >&2; }

# The runtime stage: from its FROM line to the end of the file.
RUNTIME="$(awk '/^FROM .* AS runtime$/ {p=1} p' "$DOCKERFILE")"
[ -n "$RUNTIME" ] || { echo "FAIL: no runtime stage in $DOCKERFILE" >&2; exit 1; }

jobs_found=0
for script in "$REPO"/scripts/*-job.sh; do
  [ -e "$script" ] || continue
  jobs_found=$((jobs_found + 1))
  base="$(basename "$script" .sh)"
  bin="/usr/local/bin/$base"
  if grep -qE "^COPY --from=builder /build/scripts/$base\.sh $bin\$" <<<"$RUNTIME"; then
    ok "$base is copied to $bin in the runtime stage"
  else
    bad "$base.sh is not copied to $bin in the runtime stage of $DOCKERFILE"
  fi
  if grep -qE "test -x $bin( |\$)" <<<"$RUNTIME"; then
    ok "$base: the image build asserts $bin is executable"
  else
    bad "$base: no 'test -x $bin' build assertion in the runtime stage"
  fi
  if [ -x "$script" ]; then ok "$base.sh is executable in the checkout"; else bad "$base.sh is not executable (git mode 100755)"; fi
done
[ "$jobs_found" -ge 2 ] && ok "found $jobs_found job scripts" || bad "expected at least migrate-kind-job.sh and events-copy-job.sh, found $jobs_found"

echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
