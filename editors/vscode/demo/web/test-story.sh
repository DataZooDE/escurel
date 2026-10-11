#!/bin/sh
# Equivalence / behaviour test for the unified demo story script (story.sh).
#
# OLD vs NEW: the pre-merge story scripts (plain story.sh, s2d story-s2d.sh, extracted from git HEAD
# while they still exist there) and the unified script run in both modes with a stub `node` on PATH
# that records every invocation (argv, cwd, the ESCUREL_DEMO_* environment); the recorded command lines
# are diffed, plus the marker behaviour: plain replays every run (re-running it on the same data
# duplicates the story), s2d stops at its story.done marker (a second run does nothing). The NEW traces
# are also asserted as a locked contract, so the test keeps guarding the behaviour after the merge,
# when HEAD no longer carries the old scripts (that leg is then skipped with a note). Also proves the
# default mode is plain and that DEMO_STORY_DRY_RUN=1 prints without touching anything.
#
#   sh editors/vscode/demo/web/test-story.sh
#
# STORY_UNDER_TEST overrides the script under test (default: the sibling
# story.sh); used to show the test goes red against a broken variant.
set -eu
HERE=$(dirname "$0")
STORY=${STORY_UNDER_TEST:-$HERE/story.sh}
SRC=$(cd "$HERE/.." && pwd)
ROOT=$(git rev-parse --show-toplevel 2>/dev/null || true)

pass=0; fail=0
ok() { pass=$((pass + 1)); echo "ok: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1"; }

command -v node >/dev/null || { echo "FAIL: node not on PATH"; exit 1; }
sh -n "$STORY" || { echo "FAIL: sh -n rejects $STORY"; exit 1; }
ok "sh -n accepts $STORY"

T=""; NP=""; NS=""; OP=""; OS=""; B=""; E=""
trap 'rm -rf "$T" "$NP" "$NS" "$OP" "$OS" "$B" "$E"' EXIT
T=$(mktemp -d)
mkdir -p "$T/bin"
# Recorder stub for node: logs cwd, the demo env, and argv; always succeeds (so the until-loops pass).
cat > "$T/bin/node" <<'EOF'
#!/bin/sh
{
  echo '--- call ---'
  echo "cwd: $PWD"
  echo "ratings: ${ESCUREL_DEMO_RATINGS_URL:-}"
  echo "confirmations: ${ESCUREL_DEMO_CONFIRMATIONS_URL:-}"
  echo "wait: ${ESCUREL_DEMO_WAIT_MS:-}"
  echo "orders-secret: ${ESCUREL_DEMO_ORDERS_DB_SECRET:-}"
  printf 'args: %s\n' "$*"
} >> "$NODE_LOG"
exit 0
EOF
chmod +x "$T/bin/node"
# Lays out a demo dir: the gateway/bearer [REDACTED] the s2d waits need, plus an S2D fixture.
mkdemo() {
  mkdir -p "$1/state" "$1/s2d/pages"
  printf '{"gateway_url":"http://127.0.0.1:45451","admin_bearer":"adm0","tenant":"demo"}\n' > "$1/state/gateway.json"
  printf '{"bearer":"b0","admin_bearer":"adm0"}\n' > "$1/state/bearer.json"
  printf 'hetzner-2026-10-11\n' > "$1/s2d/STAMP"
}
# Runs a story script ($1) against a demo dir ($2) in a mode ($3), node log to $4, stdout to $5; prints rc.
do_run() {
  PATH="$T/bin:$PATH" NODE_LOG="$4" DEMO_SRC="$SRC" DEMO_DIR="$2" DEMO_STORY_MODE="$3" S2D_DIR="$2/s2d" \
    sh "$1" > "$5" 2>&1
  printf '%s' "$?"
}
# Normalizes demo-dir, s2d-dir and src paths in a recorded log ($1) to @D/@S2D/@SRC.
norm() { sed -e "s|$3|@S2D|g" -e "s|$2|@D|g" -e "s|$SRC|@SRC|g" "$1"; }
calls() { grep -c '^--- call ---$' "$1"; }

NP=$(mktemp -d); mkdemo "$NP"
NS=$(mktemp -d); mkdemo "$NS"

# --- plain mode (the old story.sh): two node calls straight into the pre-written gateway state.
rc=$(do_run "$STORY" "$NP" plain "$T/np1.log" "$T/np1.out")
[ "$rc" = 0 ] && ok "plain first run exits 0" || bad "plain first run exits 0 (rc=$rc)"
[ "$(calls "$T/np1.log")" = 2 ] && ok "plain makes two node calls" || bad "plain makes two node calls"
norm "$T/np1.log" "$NP" "$NP/s2d" > "$T/np1.norm"
grep -q '^args: materialise.mjs @D/state/gateway.json$' "$T/np1.norm" \
  && ok "plain materialises the pre-written gateway state" || bad "plain materialises the pre-written gateway state"
grep -q '^args: driver.mjs @D/state/gateway.json @D/state/bearer.json$' "$T/np1.norm" \
  && ok "plain drives the pre-written gateway state" || bad "plain drives the pre-written gateway state"
grep -q '^cwd: @SRC$' "$T/np1.norm" && ok "plain runs from the demo source dir" || bad "plain runs from the demo source dir"
grep -q '^ratings: http://127.0.0.1:9101$' "$T/np1.norm" \
  && grep -q '^confirmations: http://127.0.0.1:9102/mcp$' "$T/np1.norm" \
  && grep -q '^orders-secret: @D/secrets/demo/orders-db$' "$T/np1.norm" \
  && grep -q '^wait: $' "$T/np1.norm" \
  && ok "plain exports the outside-system env (no WAIT_MS)" || bad "plain exports the outside-system env (no WAIT_MS)"
# Plain has no marker: a second run replays (duplicating the story, like the old script).
rc=$(do_run "$STORY" "$NP" plain "$T/np2.log" "$T/np2.out")
[ "$rc" = 0 ] && [ "$(calls "$T/np2.log")" = 2 ] \
  && ok "plain second run replays (no marker)" || bad "plain second run replays (no marker)"

# --- default mode is plain.
B=$(mktemp -d); mkdemo "$B"
PATH="$T/bin:$PATH" NODE_LOG="$T/def.log" DEMO_SRC="$SRC" DEMO_DIR="$B" S2D_DIR="$B/s2d" \
  sh "$STORY" > "$T/def.out" 2>&1
norm "$T/def.log" "$B" "$B/s2d" > "$T/def.norm"
[ "$(diff "$T/np1.norm" "$T/def.norm" && echo same)" = same ] \
  && ok "default mode matches explicit plain" || bad "default mode matches explicit plain"

# --- s2d mode (the old story-s2d.sh): waits, live bearers, story plus the S2D demo, marker.
rc=$(do_run "$STORY" "$NS" s2d "$T/ns1.log" "$T/ns1.out")
[ "$rc" = 0 ] && ok "s2d first run exits 0" || bad "s2d first run exits 0 (rc=$rc)"
[ "$(calls "$T/ns1.log")" = 7 ] && ok "s2d makes seven node calls" || bad "s2d makes seven node calls (got $(calls "$T/ns1.log"))"
norm "$T/ns1.log" "$NS" "$NS/s2d" > "$T/ns1.norm"
grep -q "fetch('http://127.0.0.1:45451/healthz')" "$T/ns1.norm" \
  && grep -q "fetch('http://127.0.0.1:8088/healthz')" "$T/ns1.norm" \
  && ok "s2d waits on the gateway port and the runner" || bad "s2d waits on the gateway port and the runner"
grep -q "fs.writeFileSync('@D/state/live.json'" "$T/ns1.norm" \
  && ok "s2d refreshes the live Bearer [REDACTED]" || bad "s2d refreshes the live Bearer [REDACTED]"
grep -q '^args: materialise.mjs @D/state/live.json$' "$T/ns1.norm" \
  && grep -q '^args: driver.mjs @D/state/live.json @D/state/bearer.json$' "$T/ns1.norm" \
  && ok "s2d plays the story into the live state" || bad "s2d plays the story into the live state"
grep -q '^args: s2d/seed.mjs @D/state/live.json @D/state/bearer.json @S2D$' "$T/ns1.norm" \
  && grep -q '^args: s2d/check-awaiting.mjs @D/state/live.json @D/state/bearer.json$' "$T/ns1.norm" \
  && ok "s2d loads the S2D demo and reports what waits" || bad "s2d loads the S2D demo and reports what waits"
grep -q '^wait: 900000$' "$T/ns1.norm" \
  && ok "s2d exports ESCUREL_DEMO_WAIT_MS" || bad "s2d exports ESCUREL_DEMO_WAIT_MS"
[ -f "$NS/state/story.done" ] && tail -1 "$T/ns1.out" | grep -q '^story played$' \
  && ok "s2d writes the story.done marker" || bad "s2d writes the story.done marker"
grep -q 'hetzner seed hetzner-2026-10-11' "$T/ns1.out" \
  && ok "s2d names the loaded hetzner seed" || bad "s2d names the loaded hetzner seed"
# Idempotent: a second run does nothing (the stub log is never even created).
rc=$(do_run "$STORY" "$NS" s2d "$T/ns2.log" "$T/ns2.out")
[ "$rc" = 0 ] && [ "$(cat "$T/ns2.out")" = "story already played" ] && [ ! -e "$T/ns2.log" ] \
  && ok "s2d second run does nothing" || bad "s2d second run does nothing"

# --- unknown mode is rejected.
if DEMO_SRC="$SRC" DEMO_DIR="$NS" DEMO_STORY_MODE=bogus sh "$STORY" 2>/dev/null; then bad "unknown mode is rejected"; else ok "unknown mode is rejected"; fi

# --- OLD vs NEW: the pre-merge scripts from git HEAD, path-adapted to temp copies.
if [ -n "$ROOT" ] && git -C "$ROOT" cat-file -e 'HEAD:editors/vscode/demo/web/story-s2d.sh' 2>/dev/null; then
  OP=$(mktemp -d); mkdemo "$OP"
  OS=$(mktemp -d); mkdemo "$OS"
  git -C "$ROOT" show 'HEAD:editors/vscode/demo/web/story.sh' \
    | sed -e "s|^D=/demo\$|D=\"$OP\"|" -e "s|^cd /demo-src\$|cd \"$SRC\"|" > "$T/old-plain.sh"
  git -C "$ROOT" show 'HEAD:editors/vscode/demo/web/story-s2d.sh' \
    | sed -e "s|^D=/demo\$|D=\"$OS\"|" -e "s|^cd /demo-src\$|cd \"$SRC\"|" > "$T/old-s2d.sh"
  sh -n "$T/old-plain.sh" && sh -n "$T/old-s2d.sh" && ok "sh -n accepts the adapted old scripts" || bad "sh -n accepts the adapted old scripts"
  do_run "$T/old-plain.sh" "$OP" plain "$T/op1.log" "$T/op1.out" >/dev/null
  do_run "$T/old-plain.sh" "$OP" plain "$T/op2.log" "$T/op2.out" >/dev/null
  norm "$T/op1.log" "$OP" "$OP/s2d" > "$T/op.norm"; norm "$T/np1.log" "$NP" "$NP/s2d" > "$T/np.norm"
  norm "$T/op2.log" "$OP" "$OP/s2d" >> "$T/op.norm"; norm "$T/np2.log" "$NP" "$NP/s2d" >> "$T/np.norm"
  if diff -u "$T/op.norm" "$T/np.norm" > "$T/plain.diff"; then ok "plain: old vs new command lines identical"; else bad "plain: old vs new command lines identical ($(cat "$T/plain.diff"))"; fi
  do_run "$T/old-s2d.sh" "$OS" s2d "$T/os1.log" "$T/os1.out" >/dev/null
  do_run "$T/old-s2d.sh" "$OS" s2d "$T/os2.log" "$T/os2.out" >/dev/null
  norm "$T/os1.log" "$OS" "$OS/s2d" > "$T/os.norm"; norm "$T/ns1.log" "$NS" "$NS/s2d" > "$T/ns.norm"
  if diff -u "$T/os.norm" "$T/ns.norm" > "$T/s2d.diff"; then ok "s2d: old vs new command lines identical"; else bad "s2d: old vs new command lines identical ($(cat "$T/s2d.diff"))"; fi
  if [ "$(cat "$T/os2.out")" = "story already played" ] && [ ! -s "$T/os2.log" ] \
    && [ "$(cat "$T/ns2.out")" = "story already played" ] && [ ! -s "$T/ns2.log" ] \
    && [ "$(cat "$T/os1.out")" = "$(cat "$T/ns1.out")" ]; then
    ok "s2d: old vs new marker behaviour identical"
  else
    bad "s2d: old vs new marker behaviour identical"
  fi
else
  echo "note: old story scripts not in HEAD (post-merge); equivalence was proven at merge time, contract legs above guard it"
fi

# --- dry run prints without touching anything (waits are printed, not waited on: empty state dir).
E=$(mktemp -d)
mkdir -p "$E/s2d/pages"; printf 'hetzner-2026-10-11\n' > "$E/s2d/STAMP"
if command -v timeout >/dev/null; then TO="timeout 30"; else TO=""; fi
DEMO_SRC="$SRC" DEMO_DIR="$E" DEMO_STORY_MODE=plain DEMO_STORY_DRY_RUN=1 $TO sh "$STORY" > "$T/dry-plain.out" 2>&1
rc=$?
[ "$rc" = 0 ] && grep -q '^+ node materialise.mjs .*/state/gateway.json$' "$T/dry-plain.out" \
  && grep -q '^+ node driver.mjs .* > .*/state/story.json$' "$T/dry-plain.out" \
  && grep -q '^+ cat .*/state/story.json$' "$T/dry-plain.out" \
  && grep -q '^+ export ESCUREL_DEMO_RATINGS_URL=http://127.0.0.1:9101$' "$T/dry-plain.out" \
  && [ ! -e "$E/state" ] \
  && ok "plain dry run prints without touching anything" || bad "plain dry run prints without touching anything (rc=$rc)"
DEMO_SRC="$SRC" DEMO_DIR="$E" DEMO_STORY_MODE=s2d DEMO_STORY_DRY_RUN=1 S2D_DIR="$E/s2d" $TO sh "$STORY" > "$T/dry-s2d.out" 2>&1
rc=$?
[ "$rc" = 0 ] && grep -q '^+ until \[ -s .*/state/gateway.json' "$T/dry-s2d.out" \
  && grep -q '^+ node -e' "$T/dry-s2d.out" \
  && grep -q 'live.json' "$T/dry-s2d.out" \
  && grep -q 's2d/seed.mjs' "$T/dry-s2d.out" \
  && grep -q '| tee .*/state/awaiting.txt' "$T/dry-s2d.out" \
  && grep -q '^+ touch .*/state/story.done$' "$T/dry-s2d.out" \
  && [ ! -e "$E/state" ] \
  && ok "s2d dry run prints without touching anything" || bad "s2d dry run prints without touching anything (rc=$rc)"
mkdir -p "$E/state"; touch "$E/state/story.done"
out=$(DEMO_SRC="$SRC" DEMO_DIR="$E" DEMO_STORY_MODE=s2d DEMO_STORY_DRY_RUN=1 sh "$STORY")
if [ "$(printf '%s\n' "$out" | tail -1)" = "story already played" ] \
  && ! printf '%s\n' "$out" | grep -q 'node\|touch'; then
  ok "s2d dry run honours the marker"
else
  bad "s2d dry run honours the marker"
fi

echo "---"
echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
