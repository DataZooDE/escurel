#!/usr/bin/env bash
# Re-capture the wire-contract golden files from a real gateway + runner. See refresh-golden.mjs.
#   cargo build --release -p escurel-test-support -p escurel-runner --bins && scripts/refresh-golden.sh
set -euo pipefail
cd "$(dirname "$0")/.."
exec node scripts/refresh-golden.mjs "$@"
