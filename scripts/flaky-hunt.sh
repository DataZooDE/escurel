#!/usr/bin/env bash
# Run the tests known to flake under load N times each and report pass/fail counts, so a flake is a number and
# not an anecdote. Not part of CI (it takes a long time and its point is to run on a busy machine).
#
#   scripts/flaky-hunt.sh                 10 runs of every test below
#   scripts/flaky-hunt.sh 20              20 runs
#   scripts/flaky-hunt.sh 10 connect_retry binary_boots     only these (by file name)
#   FLAKY_REPO=~/wt-escurel/other scripts/flaky-hunt.sh     a checkout whose target/ is already built
#
# A test binary is built once per crate; each run is one `cargo test` invocation of the already built binary,
# filtered to the file's module (`<file>::`). Output goes to $FLAKY_OUT (default: a temp dir) per test, so a
# failing run's output can be read afterwards. Exit 1 when any test failed at least once.
set -uo pipefail

REPO="${FLAKY_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RUNS="${1:-10}"
[[ "$RUNS" =~ ^[0-9]+$ ]] || { echo "usage: $0 [runs] [test ...]" >&2; exit 2; }
shift || true
OUT="${FLAKY_OUT:-$(mktemp -d "${TMPDIR:-/tmp}/flaky-hunt.XXXXXX")}"
mkdir -p "$OUT"

# file name -> "crate". All of them are modules of their crate's single `suite` test target.
declare -A CRATE=(
  [promotion_cascades]=escurel-runner
  [runner_status]=escurel-runner
  [agent_token_narrowing]=escurel-runner
  [rows_paging_keys]=escurel-server
  [binary_boots]=escurel-server
  [connect_retry]=escurel-client
  [gateway_not_ready]=escurel-runner
)
ORDER=(promotion_cascades runner_status agent_token_narrowing gateway_not_ready rows_paging_keys binary_boots connect_retry)
if [ "$#" -gt 0 ]; then
  ORDER=("$@")
  for t in "${ORDER[@]}"; do [ -n "${CRATE[$t]:-}" ] || { echo "unknown test file: $t (known: ${!CRATE[*]})" >&2; exit 2; }; done
fi

cd "$REPO"
echo "load: $(cut -d' ' -f1-3 /proc/loadavg)   runs: $RUNS   output: $OUT"
failed_any=0
for t in "${ORDER[@]}"; do
  crate="${CRATE[$t]}"
  if ! cargo test -p "$crate" --test suite --no-run >"$OUT/$t.build.log" 2>&1; then
    echo "BUILD FAILED for $crate (see $OUT/$t.build.log)"; exit 1
  fi
  pass=0; fail=0
  for i in $(seq 1 "$RUNS"); do
    if cargo test -p "$crate" --test suite "$t::" >"$OUT/$t.$i.log" 2>&1; then
      pass=$((pass + 1)); rm -f "$OUT/$t.$i.log"
    else
      fail=$((fail + 1))
    fi
  done
  printf '%-24s %-16s pass %2d  fail %2d\n' "$t" "$crate" "$pass" "$fail"
  [ "$fail" -eq 0 ] || failed_any=1
done
echo "load after: $(cut -d' ' -f1-3 /proc/loadavg); failing runs' output is under $OUT"
exit "$failed_any"
