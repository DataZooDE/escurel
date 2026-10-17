#!/usr/bin/env bash
# Prints the `tags=` and `require_gdrive=` lines publish-image.yml appends to $GITHUB_OUTPUT.
#   REF_TYPE=tag|branch REF_NAME=<name> GIT_SHA=<40 hex> scripts/image-tags.sh ghcr.io/datazoode/escurel
# The tag name is attacker-chosen (anyone who can push a tag) and the job holds `packages: write`, so a tag is
# validated by scripts/release-version.sh BEFORE it is used, and it only ever reaches this script through the
# environment, never through `${{ }}` interpolation into shell.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
image="${1:?usage: image-tags.sh <image>}"
sha12="$(printf '%s' "${GIT_SHA:?}" | cut -c1-12)"
if [[ "${REF_TYPE:?}" == "tag" ]]; then
  "$HERE/release-version.sh" "refs/tags/${REF_NAME:?}" >/dev/null
  first="$REF_NAME"
  require_gdrive=1
else
  first="main"
  require_gdrive=0
fi
echo "require_gdrive=${require_gdrive}"
echo "tags=${image}:${first},${image}:${sha12}"
echo "::notice title=Publish::${image} :${first} + :${sha12}" >&2
