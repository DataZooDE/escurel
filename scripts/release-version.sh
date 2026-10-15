#!/usr/bin/env bash
# Prints the version a build derives from a git ref, or refuses.
#   scripts/release-version.sh "$GITHUB_REF"      -> 2026.10.15   (refs/tags/v2026.10.15)
#                                                  -> 0.0.0-dev    (any non-tag ref)
# The value reaches shell `run:` blocks in release.yml (through ${{ steps.ver.outputs.version }}), and a git tag
# name may contain `$`, `(`, `;`, a backtick or `"`: a tag such as v1$(cmd) would run in the release job, which
# holds a write token. Only a plain version is accepted: dot-separated numbers, then an optional `-prerelease`
# and an optional `+build` made of [0-9A-Za-z.].
set -euo pipefail
ref="${1:-}"
if [[ "$ref" == refs/tags/v* ]]; then
  v="${ref#refs/tags/v}"
else
  v="0.0.0-dev"
fi
if ! [[ "$v" =~ ^[0-9]+(\.[0-9]+)*(-[0-9A-Za-z.]+)?(\+[0-9A-Za-z.]+)?$ ]]; then
  echo "release-version: refusing the tag version '${v//[^[:print:]]/?}': expected digits and dots, then an optional -pre and +build (e.g. 2026.10.15 or 2026.10.15-rc1)" >&2
  exit 1
fi
printf '%s\n' "$v"
