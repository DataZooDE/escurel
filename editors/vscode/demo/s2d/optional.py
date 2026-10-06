#!/usr/bin/env python3
"""Keeps or skips the S2D query pages that need the anofox_optimize DuckDB extension.

usage: optional.py <out_dir> <extension file>

A query whose SQL calls an `opt_*` function is kept when the extension build exists (its path is written
to <out_dir>/index-extensions, which run.sh hands to the gateway as ESCUREL_INDEX_EXTENSIONS) and moved
to <out_dir>/skipped/ with a warning when it does not. Nothing here fails the demo.
"""
import re
import shutil
import sys
from pathlib import Path

out, ext = Path(sys.argv[1]), Path(sys.argv[2])
queries = out / "pages" / "queries"
needs = [q for q in sorted(queries.glob("*.md")) if re.search(r"\bopt_[a-z_]+\s*\(", q.read_text())]
if needs and ext.is_file():
    (out / "index-extensions").write_text(str(ext) + "\n")
    print(f"s2d: {len(needs)} query page(s) use anofox_optimize: loading {ext}")
elif needs:
    skipped = out / "skipped"
    skipped.mkdir(exist_ok=True)
    for q in needs:
        shutil.move(str(q), skipped / q.name)
    names = ", ".join(q.stem for q in needs)
    print(f"s2d: WARNING skipped {len(needs)} query page(s) that need the anofox_optimize extension ({names}); "
          f"no build at {ext}", file=sys.stderr)
