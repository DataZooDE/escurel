# escurel — Docker Compose (generic single-node)

The substrate-free baseline: the same `escurel-server` image running on a
cloud VM, bare metal, or a laptop, with state on a named volume. One
container serves one tenant.

```sh
cp .env.example .env          # edit auth / embedding / ports
docker compose up -d --build
curl -fsS localhost:8080/healthz     # -> OK
curl -fsS localhost:8080/readyz      # -> {"ready":true,...} once the embedder is up
```

- **State** lives in the `escurel-data` volume at `/data` (DuckDB derived
  index + the canonical markdown LaneStore + blobs).
- **One replica, STOP-FIRST.** DuckDB is single-writer; do not scale or roll.
- **Config** is all `ESCUREL_*` env in `.env` — see `.env.example` and
  [`../../docs/deploy/README.md`](../../docs/deploy/README.md) for the full
  surface. Unset OIDC issuer ⇒ unauthenticated dev mode.
- **Backups**: snapshot the volume, or use the logical per-tenant
  `tenant_export` admin tool (markdown, not a full `/data` image).

## Smoke test

`./smoke.sh` builds the image, brings the stack up, asserts `/healthz`, and
verifies the corpus survives a restart. Exit code 0 = pass. Needs Docker +
the Compose plugin.
