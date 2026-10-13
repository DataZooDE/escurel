#!/usr/bin/env bash
# Gate a release tag on the MAIN push CI run of the tagged commit, checked early (release.yml `gate` job).
#
#   scripts/release-gate.sh <commit-sha> [--repo owner/name] [--dry-run <fixture.json>]
#
# Passes (exit 0) when
#   1. <commit-sha> is an ancestor of origin/main (a tag on a side branch is refused), and
#   2. the newest `CI` workflow run of the `push` event on `main` for exactly that commit has
#      completed with conclusion `success` AND its `fmt + clippy + test` job succeeded.
# A red or incomplete result fails fast on red and WAITS on missing/in-progress (the push run may
# still be running when the tag is pushed) until RELEASE_GATE_TIMEOUT_SECS, then fails.
#
# Environment:
#   RELEASE_SKIP_CI_GATE=1       emergency escape (the repository variable of the same name): skips every
#                                check, loudly. Only the exact value 1 counts.
#   RELEASE_GATE_TIMEOUT_SECS    how long to wait for the push run (default 1800)
#   RELEASE_GATE_POLL_SECS       poll interval (default 30)
#   RELEASE_GATE_REF             the ref the commit must be an ancestor of (default origin/main)
#   GH_TOKEN / GITHUB_REPOSITORY as for `gh`
#
# --dry-run reads {"ancestor": bool, "polls": [{"runs": [...], "jobs": [...]}, ...]} instead of git and
# the GitHub API (poll N answers with polls[min(N, last)]); see scripts/test-release-gate.sh.
set -euo pipefail

JOB_NAME="fmt + clippy + test"
usage() { sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
die() { echo "release-gate: $*" >&2; exit 2; }

SHA=""; FIXTURE=""; REPO="${GITHUB_REPOSITORY:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) [ $# -ge 2 ] || die "--dry-run needs a fixture file"; FIXTURE="$2"; shift 2 ;;
    --repo) [ $# -ge 2 ] || die "--repo needs owner/name"; REPO="$2"; shift 2 ;;
    -h|--help) usage ;;
    -*) die "unknown option: $1" ;;
    *) [ -z "$SHA" ] || die "more than one commit given"; SHA="$1"; shift ;;
  esac
done
[ -n "$SHA" ] || usage
[[ "$SHA" =~ ^[0-9a-f]{40}$ ]] || die "not a full commit sha: $SHA"
command -v jq >/dev/null || die "jq is required"

if [ "${RELEASE_SKIP_CI_GATE:-}" = "1" ]; then
  echo "::warning::RELEASE_SKIP_CI_GATE=1: the release CI gate is SKIPPED for ${SHA}. Unset the repository variable afterwards."
  echo "release-gate: RELEASE_SKIP_CI_GATE=1 -> gate skipped for ${SHA}"
  exit 0
fi

TIMEOUT="${RELEASE_GATE_TIMEOUT_SECS:-1800}"
POLL="${RELEASE_GATE_POLL_SECS:-30}"
REF="${RELEASE_GATE_REF:-origin/main}"
err() { echo "::error::$*"; echo "release-gate: $*" >&2; }

# 1. The tagged commit must be on main.
if [ -n "$FIXTURE" ]; then
  ancestor="$(jq -r 'if has("ancestor") then .ancestor else true end' "$FIXTURE")"
  [ "$ancestor" = "true" ] || { err "${SHA} is not an ancestor of ${REF}: tag a commit that is on main"; exit 1; }
else
  git merge-base --is-ancestor "$SHA" "$REF" 2>/dev/null \
    || { err "${SHA} is not an ancestor of ${REF}: tag a commit that is on main"; exit 1; }
fi

# 2. state N -> {"runs":[...],"jobs":[...]} for poll N.
state() {
  local n=$1
  if [ -n "$FIXTURE" ]; then
    jq -c --argjson n "$n" '.polls | .[ ([$n, (length - 1)] | min) ]' "$FIXTURE"
    return
  fi
  local runs id jobs='[]'
  runs="$(gh run list --repo "$REPO" --workflow CI --commit "$SHA" --branch main --event push \
    --json databaseId,status,conclusion,createdAt --limit 20)"
  id="$(jq -r 'sort_by(.createdAt) | last | select(.status == "completed") | .databaseId // empty' <<<"$runs")"
  if [ -n "$id" ]; then jobs="$(gh run view "$id" --repo "$REPO" --json jobs --jq '.jobs')"; fi
  jq -nc --argjson runs "$runs" --argjson jobs "$jobs" '{runs: $runs, jobs: $jobs}'
}

# 3. verdict: success | running | missing | failed:<why>
verdict() {
  jq -r --arg job "$JOB_NAME" '
    (.runs // []) as $r
    | if ($r | length) == 0 then "missing"
      else ($r | sort_by(.createdAt) | last) as $run
        | if $run.status != "completed" then "running"
          elif $run.conclusion != "success" then "failed:the CI run on main for this commit ended \($run.conclusion)"
          else ((.jobs // []) | map(select(.name == $job)) | last) as $j
            | if $j == null then "failed:job \"\($job)\" not found in the CI run on main for this commit"
              elif $j.conclusion != "success" then "failed:job \"\($job)\" ended \($j.conclusion)"
              else "success" end
          end
      end'
}

start="$(date +%s)"; n=0
while :; do
  v="$(state "$n" | verdict)"
  case "$v" in
    success) echo "release-gate: green: the CI run on main for ${SHA} succeeded (${JOB_NAME})"; exit 0 ;;
    failed:*) err "${v#failed:}: fix CI on main or tag a green commit"; exit 1 ;;
    running|missing)
      elapsed=$(( $(date +%s) - start ))
      if [ "$elapsed" -ge "$TIMEOUT" ]; then
        err "timed out after ${elapsed}s waiting for the CI run on main for ${SHA} (${v}): re-run the release once it is green"; exit 1
      fi
      echo "release-gate: ${v}: waiting for the CI run on main for ${SHA} (${elapsed}s of ${TIMEOUT}s)"
      sleep "$POLL"; n=$((n + 1)) ;;
    *) err "unexpected verdict: ${v}"; exit 1 ;;
  esac
done
