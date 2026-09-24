#!/usr/bin/env bash
# The Playwright visual check, run inside the pinned Playwright image so the
# fonts — and therefore the pixels — are the same on every machine and in CI.
# Baselines under test/visual/__screenshots__ are generated the same way:
#   scripts/visual.sh --update-snapshots
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="$(node -e "console.log(require('@playwright/test/package.json').version)")"
IMAGE="mcr.microsoft.com/playwright:v${VERSION}-jammy"
node esbuild.mjs --production >/dev/null
exec docker run --rm --init -v "$PWD:/work" -w /work -e CI="${CI:-}" --ipc=host "$IMAGE" npx playwright test "$@"
