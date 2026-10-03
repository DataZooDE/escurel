#!/usr/bin/env bash
# scripts/check.sh — Escurel's local gate, shared by CI and the pre-push hook.
#
#   scripts/check.sh              # the pre-push set (below)
#   scripts/check.sh fmt clippy   # named steps, in the order given
#   scripts/check.sh --list
#
# CI (.github/workflows/ci.yml, explore.yml, vscode.yml) calls the same
# steps, so a command changes here or nowhere. CI-only tuning (caches,
# CARGO_BUILD_JOBS, RUSTFLAGS) stays in the workflows. Offline apart from
# dependency and libduckdb downloads: the container-backed storage tests
# (live.yml) and anything that deploys are not here.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Formatting first: it is the cheapest failure and the most common one.
# The pre-push hook appends `explore` / `vscode` when the pushed commits touch
# those trees — the same path filters their workflows use.
PRE_PUSH=(fmt clippy clippy-features test)
# `release` is CI's build job; explore-web is explore.yml's smoke build.
EXTRA=(explore vscode release explore-web)

step_fmt() {
  cargo fmt --all -- --check || { echo "fix with: cargo fmt --all" >&2; return 1; }
}

step_clippy() {
  cargo clippy --workspace --all-targets -- -D warnings
}

# The SHIPPED feature set: s3.rs / gcs.rs are otherwise never compiled.
# No --all-targets: those backends' tests need containers (live.yml).
step_clippy_features() {
  cargo clippy -p escurel-server -p escurel-storage --features s3,gcs -- -D warnings
}

step_test() {
  cargo test --workspace --all-targets
}

step_release() {
  cargo build --workspace --release
}

FLUTTER_PIN=3.44.0 # keep in step with explore.yml's flutter-version

step_explore() {
  local v
  v="$(flutter --version 2>/dev/null | awk 'NR==1 {print $2}')"
  if [ "$v" != "$FLUTTER_PIN" ]; then
    echo "note: local Flutter $v, CI pins $FLUTTER_PIN — analyzer results may differ" >&2
  fi
  # The kit first, so a failure there is reported as the kit's.
  flutter_package packages/escurel_explorer_kit
  flutter_package apps/escurel-explore
}

# Newer Flutter SDKs rewrite tracked files on `pub get` (3.47 adds analyzer
# excludes to analysis_options.yaml). A check must not leave the tree dirty,
# so restore any tracked file under the package that was clean beforehand.
# Restore on failure too: a failed analyze must not leave the rewrite behind.
flutter_package() {
  local before after f rc=0
  before="$(git diff --name-only -- "$1")"
  (cd "$1" && flutter pub get && flutter analyze && flutter test --reporter expanded) || rc=$?
  after="$(git diff --name-only -- "$1")"
  for f in $after; do
    if ! grep -qxF "$f" <<<"$before"; then
      echo "note: flutter rewrote $f; restored it" >&2
      git checkout -- "$f"
    fi
  done
  return "$rc"
}

step_explore_web() {
  (cd apps/escurel-explore && flutter build web --release \
    --dart-define=ESCUREL_EXPLORE_MODE=http \
    --dart-define=ESCUREL_EXPLORE_VERSION="$(git rev-parse --short=7 HEAD)")
}

# typecheck + lint + unit + component tests (`npm test`). The Playwright
# visual baselines, build and VSIX packaging stay in vscode.yml.
step_vscode() {
  (cd editors/vscode && npm ci && npm test)
}

usage() {
  echo "usage: $0 [--list | step...]"
  echo "  pre-push (default): ${PRE_PUSH[*]}"
  echo "  also available:     ${EXTRA[*]}"
}

steps=()
case "${1:-pre-push}" in
  --list | -h | --help) usage; exit 0 ;;
esac
for arg in "${@:-pre-push}"; do
  if [ "$arg" = pre-push ]; then
    steps+=("${PRE_PUSH[@]}")
  elif declare -F "step_${arg//-/_}" >/dev/null; then
    steps+=("$arg")
  else
    echo "unknown step: $arg" >&2; usage >&2; exit 2
  fi
done

start=$SECONDS
for s in "${steps[@]}"; do
  t=$SECONDS
  echo "==> check: $s"
  "step_${s//-/_}"
  echo "<== check: $s ok ($((SECONDS - t))s)"
done
echo "check: ${#steps[@]} step(s) passed in $((SECONDS - start))s"
