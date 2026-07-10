# The runtime container image was missing libduckdb.so

**Date:** 2026-07-09
**Scope:** `Dockerfile` runtime stage / any container deploy

## Symptom

A freshly built `escurel-server` container crash-loops at startup with:

```
/usr/local/bin/escurel-server: error while loading shared libraries:
libduckdb.so: cannot open shared object file: No such file or directory
```

`/healthz` never comes up. Surfaced by `deploy/compose/smoke.sh`.

## Cause

The DuckDB **download** backend (`.cargo/config.toml`:
`DUCKDB_DOWNLOAD_LIB=1`, see
[`2026-06-20-duckdb-download-instead-of-build.md`](2026-06-20-duckdb-download-instead-of-build.md))
links libduckdb **dynamically**: the binary carries `NEEDED libduckdb.so`
and — critically — **no rpath/runpath** (`readelf -d` shows only the
`NEEDED` entry). At link time libduckdb-sys passes a `-L` path so the link
succeeds, but nothing bakes a runtime search path into the ELF.

`.dockerignore` does **not** exclude `.cargo/`, so download mode is active
inside the image build too — but the runtime stage copied only the binary,
not the `.so`. It "worked" on a dev box purely because the host had
`/usr/lib/libduckdb.so` installed. (The publish workflow's "bundled-DuckDB
compile" comment is stale from before the download switch.)

## Fix

Ship `libduckdb.so` in the runtime image. In the builder RUN — while the
cache-mounted `target/` is still visible — copy it out to a non-mounted
dir, then `COPY --from=builder` it into a standard loader dir and refresh
the cache:

```dockerfile
# builder (inside the RUN that has --mount=type=cache,target=/build/target)
&& cp "$(find target -name libduckdb.so -print -quit)" /usr/local/lib/libduckdb.so

# runtime
COPY --from=builder /usr/local/lib/libduckdb.so /usr/lib/libduckdb.so
RUN ldconfig
```

The `cp` MUST be in the same RUN as the build: the `target/` cache mount
is not visible to a later `COPY --from=builder target/...`.

## Recognise it by

`ldd /usr/local/bin/escurel-server` printing `libduckdb.so => not found`
inside the container, or the loader error above at boot. Any time the
DuckDB link mode changes (download ↔ bundled/static), re-check whether the
runtime image needs the `.so`: bundled/static needs nothing extra;
download needs the shared object shipped.
