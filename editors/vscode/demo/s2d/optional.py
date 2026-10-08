#!/usr/bin/env python3
"""Keeps or skips the S2D query pages that need the anofox_optimize DuckDB extension.

usage: optional.py <out_dir> [<the gateway's DuckDB version, e.g. 1.5.5>]

A query whose SQL calls an `opt_*` function needs the extension, and a DuckDB extension is built for ONE
DuckDB version. The file is chosen from ESCUREL_DEMO_OPT_EXT (one path) or the default builds below: the
first whose footer names the gateway's version. Its path goes to <out_dir>/index-extensions, which run.sh
hands to the gateway as ESCUREL_INDEX_EXTENSIONS. With no matching file the pages are moved to
<out_dir>/skipped/ with a warning. Nothing here fails the demo.
"""
import os
import re
import shutil
import sys
from pathlib import Path

HOME = Path.home()
DEFAULTS = [
    HOME / ".cache/escurel-scratch/ext-1.5.6/anofox_optimize.duckdb_extension",
    HOME / "Projects/datazoo/anofox-optimize/build/release/extension/anofox_optimize/anofox_optimize.duckdb_extension",
]


def footer_version(path: Path) -> str | None:
    """The DuckDB version an extension file was built for (`v1.5.5` in its footer), or None."""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 1024))
            tail = f.read()
    except OSError:
        return None
    m = re.search(rb"v(\d+\.\d+\.\d+)\x00", tail) or re.search(rb"v(\d+\.\d+\.\d+)", tail)
    return m.group(1).decode() if m else None


out = Path(sys.argv[1])
gateway = (sys.argv[2] if len(sys.argv) > 2 else "").lstrip("v")
queries = out / "pages" / "queries"
needs = [q for q in sorted(queries.glob("*.md")) if re.search(r"\bopt_[a-z_]+\s*\(", q.read_text())]
if not needs:
    sys.exit(0)

env = os.environ.get("ESCUREL_DEMO_OPT_EXT") or os.environ.get("ESCUREL_DEMO_OPTIMIZE_EXT")
candidates = [Path(env)] if env else DEFAULTS
found = [(p, footer_version(p)) for p in candidates if p.is_file()]
chosen = next((p for p, v in found if gateway and v == gateway), None)
if chosen:
    (out / "index-extensions").write_text(str(chosen) + "\n")
    print(f"s2d: {len(needs)} query page(s) use anofox_optimize: loading {chosen} (DuckDB v{gateway})")
else:
    skipped = out / "skipped"
    skipped.mkdir(exist_ok=True)
    for q in needs:
        shutil.move(str(q), skipped / q.name)
    names = ", ".join(q.stem for q in needs)
    if not found:
        why = "no extension build found (" + ", ".join(str(c) for c in candidates) + ")"
    elif not gateway:
        why = "the gateway's DuckDB version is unknown"
    else:
        why = f"none of the builds is for the gateway's DuckDB v{gateway} (found: " + ", ".join(f"{p.name} for v{v}" for p, v in found) + ")"
    print(f"s2d: WARNING skipped {len(needs)} query page(s) that need the anofox_optimize extension ({names}): {why}", file=sys.stderr)
