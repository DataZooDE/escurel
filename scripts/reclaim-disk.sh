#!/usr/bin/env bash
#
# reclaim-disk.sh — reclaim build-artifact disk in this repo and its worktrees.
#
# Why this exists: the workspace links ~287 test/bin executables, so a single
# built worktree costs tens of GB, and the agent fan-out creates one worktree
# per task. Left alone this reached 845 GB (2026-08-02). `target/` is pure
# regenerable cache; worktrees whose branch already merged are pure waste.
#
# Usage:
#   scripts/reclaim-disk.sh                      # report only, changes nothing
#   scripts/reclaim-disk.sh --targets            # drop cold target/ dirs
#   scripts/reclaim-disk.sh --targets --older-than 3
#   scripts/reclaim-disk.sh --merged             # remove merged worktrees
#   scripts/reclaim-disk.sh --all --yes          # both, no prompt
#
# Safety: --merged removes a worktree ONLY when all of these hold:
#   * it is not the main worktree
#   * HEAD is on a named branch (detached HEADs are reported, never removed)
#   * the working tree is clean (no uncommitted changes)
#   * the branch is an ancestor of origin/main (i.e. genuinely merged, which
#     also implies nothing is unpushed)
# Anything failing a guard is reported and skipped.

set -euo pipefail

OLDER_THAN=7
DO_TARGETS=0
DO_MERGED=0
ASSUME_YES=0

while [ $# -gt 0 ]; do
	case "$1" in
	--targets) DO_TARGETS=1 ;;
	--merged) DO_MERGED=1 ;;
	--all)
		DO_TARGETS=1
		DO_MERGED=1
		;;
	--older-than)
		OLDER_THAN="${2:?--older-than needs a number of days}"
		shift
		;;
	--yes | -y) ASSUME_YES=1 ;;
	-h | --help)
		sed -n '2,30p' "$0"
		exit 0
		;;
	*)
		echo "unknown flag: $1" >&2
		exit 2
		;;
	esac
	shift
done

cd "$(git rev-parse --show-toplevel)"
MAIN_WT="$(pwd)"

human() { # MB -> human
	local mb=$1
	if [ "$mb" -ge 1024 ]; then echo "$((mb / 1024)) GB"; else echo "${mb} MB"; fi
}

size_mb() { du -sm "$1" 2>/dev/null | cut -f1 || echo 0; }

# Age in days of the most recently written file in a tree (0 if unknown).
age_days() {
	local newest
	newest=$(find "$1" -maxdepth 3 -type f -printf '%T@\n' 2>/dev/null | sort -rn | head -1)
	[ -z "$newest" ] && {
		echo 9999
		return
	}
	echo $(((($(date +%s) - ${newest%.*})) / 86400))
}

echo "==> refreshing origin/main"
git fetch origin --quiet 2>/dev/null || echo "    (fetch failed; merged-checks use the local origin/main)"

# Collect worktrees: path<TAB>branch-or-DETACHED
WORKTREES=$(git worktree list --porcelain | awk '
	/^worktree /   { path=substr($0,10); branch="DETACHED" }
	/^branch /     { sub("^branch refs/heads/",""); branch=$0 }
	/^$/           { if (path!="") print path "\t" branch; path="" }
	END            { if (path!="") print path "\t" branch }
')

total_target_mb=0
reclaimable_mb=0
declare -a COLD_TARGETS=()
declare -a MERGED_WTS=()

printf '\n%-42s %-34s %9s %7s %s\n' "WORKTREE" "BRANCH" "TARGET" "AGE" "STATE"
printf '%.0s-' {1..110}
echo

while IFS=$'\t' read -r wt branch; do
	[ -z "$wt" ] && continue
	short="${wt#"$MAIN_WT"/}"
	[ "$wt" = "$MAIN_WT" ] && short="(main)"

	tmb=0
	age="-"
	if [ -d "$wt/target" ]; then
		tmb=$(size_mb "$wt/target")
		age="$(age_days "$wt/target")d"
		total_target_mb=$((total_target_mb + tmb))
	fi

	state=""
	dirty=$(git -C "$wt" status --porcelain 2>/dev/null | wc -l)
	if [ "$branch" = "DETACHED" ]; then
		state="detached"
	elif git -C "$wt" merge-base --is-ancestor "$branch" origin/main 2>/dev/null; then
		state="MERGED"
		if [ "$wt" != "$MAIN_WT" ] && [ "$dirty" -eq 0 ]; then
			MERGED_WTS+=("$wt")
		elif [ "$dirty" -ne 0 ]; then
			state="MERGED(dirty)"
		fi
	else
		ahead=$(git -C "$wt" rev-list --count "origin/main..$branch" 2>/dev/null || echo "?")
		state="unmerged +$ahead"
	fi
	[ "$dirty" -ne 0 ] && state="$state, ${dirty} dirty"

	# A target/ is "cold" if untouched for OLDER_THAN days.
	if [ -d "$wt/target" ] && [ "${age%d}" -ge "$OLDER_THAN" ]; then
		COLD_TARGETS+=("$wt/target")
		reclaimable_mb=$((reclaimable_mb + tmb))
	fi

	printf '%-42s %-34s %9s %7s %s\n' \
		"${short:0:42}" "${branch:0:34}" "$([ "$tmb" -gt 0 ] && human "$tmb" || echo "-")" "$age" "$state"
done <<<"$WORKTREES"

echo
echo "total target/ across worktrees : $(human "$total_target_mb")"
echo "cold (>=${OLDER_THAN}d, reclaimable) : $(human "$reclaimable_mb")  in ${#COLD_TARGETS[@]} dir(s)"
echo "merged worktrees removable     : ${#MERGED_WTS[@]}"

if [ "$DO_TARGETS" -eq 0 ] && [ "$DO_MERGED" -eq 0 ]; then
	echo
	echo "report only — pass --targets / --merged / --all to act."
	exit 0
fi

confirm() {
	[ "$ASSUME_YES" -eq 1 ] && return 0
	read -r -p "$1 [y/N] " a
	[ "$a" = "y" ] || [ "$a" = "Y" ]
}

if [ "$DO_TARGETS" -eq 1 ] && [ ${#COLD_TARGETS[@]} -gt 0 ]; then
	echo
	if confirm "Delete ${#COLD_TARGETS[@]} cold target/ dir(s), freeing $(human "$reclaimable_mb")?"; then
		for t in "${COLD_TARGETS[@]}"; do
			rm -rf "$t" && echo "  cleaned $t"
		done
	fi
fi

if [ "$DO_MERGED" -eq 1 ] && [ ${#MERGED_WTS[@]} -gt 0 ]; then
	echo
	printf '  %s\n' "${MERGED_WTS[@]}"
	if confirm "Remove the ${#MERGED_WTS[@]} merged+clean worktree(s) above?"; then
		for w in "${MERGED_WTS[@]}"; do
			git worktree remove "$w" && echo "  removed $w"
		done
	fi
fi

git worktree prune
echo
echo "==> done. repo now: $(human "$(size_mb "$MAIN_WT")")"
