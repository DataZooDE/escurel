#!/usr/bin/env python3
"""The DuckDB version a build is pinned to, read from libduckdb-sys in Cargo.lock: 1.10506.0 -> 1.5.6."""
import re
import sys

text = open(sys.argv[1]).read()
m = re.search(r'name = "libduckdb-sys"\nversion = "(\d+)\.(\d+)\.(\d+)', text)
if not m:
    sys.exit(1)
major, packed = int(m.group(1)), int(m.group(2))
print(f"{major}.{(packed // 100) % 100}.{packed % 100}")
