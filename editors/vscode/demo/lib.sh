# Sourced by run.sh (and by its tests). Two things the demo script must get right and used to get wrong.

# Is DIR safe to delete and start over? `run.sh start` wipes ESCUREL_DEMO_HOME, and that variable is
# user-settable: pointing it at a directory that is not a demo home must not cost its contents.
# Safe: absent, empty, marked by an earlier start, or carrying the demo's own files (homes made before the
# marker existed). Never "/" or $HOME.
demo_home_resettable() {
  local d="${1:-}"
  [ -n "$d" ] || return 1
  case "$d" in "/"|"$HOME"|"$HOME/") return 1 ;; esac
  [ -e "$d" ] || return 0
  [ -d "$d" ] || return 1
  [ -z "$(ls -A "$d" 2>/dev/null)" ] && return 0
  [ -f "$d/.escurel-demo-home" ] && return 0
  if [ -d "$d/profile" ] && { [ -f "$d/gateway.json" ] || [ -f "$d/code.log" ] || [ -f "$d/story.json" ]; }; then return 0; fi
  return 1
}

# A DuckDB extension is built for ONE DuckDB version, and the gateway links libduckdb.so: the copy the build
# downloaded for the version it is pinned to (target/duckdb-download/<triple>/<version>/). Prints that
# directory (ESCUREL_DEMO_LIBDUCKDB_DIR overrides). When the pinned version is known but its copy is
# missing it prints nothing: falling back to "the newest download" once ran the demo on a stale 1.5.5.
# Only when the pin cannot be read at all is the newest download used.
# Needs: GATEWAY_BIN, HERE (the demo dir), REPO.
libduckdb_dir() {
  [ -n "${ESCUREL_DEMO_LIBDUCKDB_DIR:-}" ] && { echo "$ESCUREL_DEMO_LIBDUCKDB_DIR"; return; }
  local d want
  want="$(python3 "$HERE/s2d/pinned_duckdb.py" "$REPO/Cargo.lock" 2>/dev/null || true)"
  if [ -n "$want" ]; then
    for d in "$(dirname "$GATEWAY_BIN")"/../duckdb-download/*/"$want"/; do
      [ -f "${d}libduckdb.so" ] && { echo "${d%/}"; return; }
    done
    return 0
  fi
  for d in $(ls -d "$(dirname "$GATEWAY_BIN")"/../duckdb-download/*/*/ 2>/dev/null | sort -V -r); do
    [ -f "${d}libduckdb.so" ] && { echo "${d%/}"; return; }
  done
}
