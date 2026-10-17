#!/usr/bin/env bash
# Creates the annotated release tag LOCALLY from the CHANGELOG section (never pushes). Usage, from the repo root:
#   scripts/release-tag.sh vYYYY.MM.DD [commit]        (commit defaults to HEAD)
# `git tag -a -m/-F` strips every line that starts with '#', which removes the markdown headings of the
# section (`## vX — BREAKING (...)`, `### BREAKING`) and with them the word release.yml checks for. The
# message is therefore written with --cleanup=whitespace, and the result is verified before it is kept.
# Tested by scripts/test-release-tag.sh. See docs/notes/discovered/2026-10-17-git-tag-strips-markdown-headings.md.
set -euo pipefail
TAG="${1:?usage: release-tag.sh vYYYY.MM.DD [commit]}"
COMMIT="${2:-HEAD}"
SECTION="$(awk -v t="## ${TAG}" 'index($0, t) == 1 {p=1; print; next} /^## / {p=0} p' CHANGELOG.md)"
if [[ -z "${SECTION//[[:space:]]/}" ]]; then
  echo "release-tag: CHANGELOG.md has no '## ${TAG}' section (rename '## Unreleased' first)" >&2; exit 1
fi
git tag -a --cleanup=whitespace "$TAG" "$COMMIT" -m "$SECTION"
if grep -q 'BREAKING' <<<"$SECTION" && ! git for-each-ref "refs/tags/${TAG}" --format='%(contents)' | grep -q 'BREAKING'; then
  git tag -d "$TAG" >/dev/null
  echo "release-tag: the stored tag message lost BREAKING; tag removed" >&2; exit 1
fi
echo "created annotated tag ${TAG} on $(git rev-parse --short "${COMMIT}^{commit}") (not pushed)"
