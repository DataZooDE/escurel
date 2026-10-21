#!/usr/bin/env bash
# Prints the SemVer form of a release version (the output of scripts/release-version.sh), the one Cargo and the VSIX share.
#   2026.08.02        -> 2026.8.2          (SemVer rejects leading zeros)
#   2026.10.15.1      -> 2026.10.15+1      (a same-day re-cut has a fourth component: build metadata keeps it)
#   2026.08.02.3-rc1  -> 2026.8.2-rc1+3
# Cargo.toml's version and the VSIX's `npm version` take this value; before it was shared, only Cargo handled the fourth
# component and a `vYYYY.MM.DD.N` tag failed the VSIX job. Tested by scripts/test-release-semver.sh.
set -euo pipefail
python3 - "${1:?usage: release-semver.sh VERSION}" <<'PY'
import re, sys
v = sys.argv[1]
m = re.match(r"^([0-9]+(?:\.[0-9]+)*)(-[0-9A-Za-z.-]+)?(?:\+([0-9A-Za-z.-]+))?$", v)
if not m:
    sys.exit(f"release-semver: not a release version: {v!r}")
parts = [str(int(p)) for p in m.group(1).split(".")]
core, extra = parts[:3], parts[3:]
while len(core) < 3:
    core.append("0")
build = ".".join(extra + ([m.group(3)] if m.group(3) else []))
print(".".join(core) + (m.group(2) or "") + (("+" + build) if build else ""))
PY
