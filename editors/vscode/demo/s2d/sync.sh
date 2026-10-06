#!/usr/bin/env bash
# Builds the demo's Source-to-Deliver (S2D) data from the hetzner-agent-substrate seed, which is the
# SINGLE SOURCE: nothing from it is committed here.
#
#   sync.sh <out_dir>
#
# 1. fetches origin/main of the hetzner repo (read-only) and unpacks ops/seed/s2d-demo
# 2. migrates its pages `type:` -> `kind:` on a COPY (`escurel admin migrate-kind-files --apply`;
#    the lab still runs the old format, so the source stays as it is)
# 3. builds the deterministic parquet tables (build_data.py, pure DuckDB, no cloud)
# 4. points each sql_view skill at the local parquet and applies the demo overlay (overlay.py)
#
# Output: <out_dir>/pages/{skills,reports,queries}, <out_dir>/data/<table>/<table>.parquet,
# <out_dir>/STAMP (the hetzner commit this was built from).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
OUT="${1:?usage: sync.sh <out_dir>}"
SRC_REPO="${ESCUREL_DEMO_S2D_REPO:-$HOME/Projects/datazoo/hetzner-agent-substrate}"
CLI="${ESCUREL_CLI_BIN:-$REPO/target/release/escurel}"
S3_PREFIX='s3://datazoo-substrate-app-lab/demo/s2d/'

[ -d "$SRC_REPO/.git" ] || { echo "s2d: no hetzner-agent-substrate checkout at $SRC_REPO (set ESCUREL_DEMO_S2D_REPO)" >&2; exit 1; }
[ -x "$CLI" ] || { echo "s2d: missing $CLI (cargo build --release -p escurel-cli)" >&2; exit 1; }

# Read-only: a fetch, then `git archive` of main. The checkout's working tree is never touched.
git -C "$SRC_REPO" fetch -q origin main 2>/dev/null || echo "s2d: could not fetch (offline?), using the last fetched main" >&2
SHA="$(git -C "$SRC_REPO" rev-parse --short=10 origin/main)"

rm -rf "$OUT"
mkdir -p "$OUT/src" "$OUT/pages" "$OUT/data"
git -C "$SRC_REPO" archive origin/main ops/seed/s2d-demo | tar -x -C "$OUT/src"
SEED="$OUT/src/ops/seed/s2d-demo"

# The pages, copied out of the snapshot, then migrated.
mkdir -p "$OUT/pages/skills" "$OUT/pages/reports" "$OUT/pages/queries"
cp "$SEED"/skills/*.md "$OUT/pages/skills/"
cp "$SEED"/reports/*.md "$OUT/pages/reports/"
cp "$SEED"/instances/query/*.md "$OUT/pages/queries/"
for d in skills reports queries; do
  "$CLI" admin migrate-kind-files --path "$OUT/pages/$d" --apply >/dev/null
done

# The data: deterministic, so a rerun is byte-identical.
python3 "$SEED/build_data.py" "$OUT/data" >/dev/null

# The relation of each sql_view skill is a literal bucket path in the source.
sed -i "s|$S3_PREFIX|$OUT/data/|" "$OUT"/pages/skills/*.md

# Query pages that call the anofox_optimize functions (opt_*) need that DuckDB extension in the gateway.
# With the extension build present they are kept and the gateway is told to load it
# (`index-extensions`); without it they are SKIPPED with a warning: the core stories never depend on them.
OPT_EXT="${ESCUREL_DEMO_OPTIMIZE_EXT:-$HOME/Projects/datazoo/anofox-optimize/build/release/extension/anofox_optimize/anofox_optimize.duckdb_extension}"
python3 "$HERE/optional.py" "$OUT" "$OPT_EXT"

python3 "$HERE/overlay.py" "$OUT/pages"
cp "$SEED/check_queries.py" "$SEED/build_data.py" "$OUT/"
printf '%s\n' "$SHA" > "$OUT/STAMP"
echo "s2d: built from hetzner-agent-substrate $SHA into $OUT"
