# Deploying escurel

This directory holds the deployment artefacts for `escurel-server`.
The core spec under [`../spec/`](../spec/) is deployment-target-
agnostic; the files here bind it to concrete targets.

| File | What it is |
|---|---|
| [`substrate.md`](substrate.md) | The DataZoo Hetzner substrate binding (naming, identity, storage, ingress, backup, placement). Read this for the substrate target. |
| [`escurel.nomad.hcl`](escurel.nomad.hcl) | Nomad jobspec for the substrate target — `dz-escurel`, single-replica stateful service. |
| [`escurel.pkr.hcl`](escurel.pkr.hcl) | Golden-image bake **fragment** (candle + EmbeddingGemma + DuckDB extensions + the binary). A fragment of the substrate's Packer pipeline, not a standalone build. |
| [`escurel-export-shipper.nomad.hcl`](escurel-export-shipper.nomad.hcl) | Periodic tenant-export shipper — the substrate-side half of the backup contract. |
| [`escurel.tailscale-acl.json`](escurel.tailscale-acl.json) | Tailscale ACL fragment (forward-looking per-app tag). |
| [`escurel-explore.nomad.hcl`](escurel-explore.nomad.hcl) | The Flutter editor companion app (separate workload). |

> **Two naming surfaces.** The **binary surface** keeps the project
> name: `ESCUREL_*` env vars, `escurel.toml`. The **substrate surface**
> uses the shared `dz-escurel` / `apps-dz` / `datazoo-substrate-app-<env>/dz/escurel/`
> convention so substrate tooling sees one product. See
> [`substrate.md § Naming convention`](substrate.md).

## Configuration surface

One TOML file (`${ESCUREL_CONFIG:-/etc/escurel/server.toml}`), with
`ESCUREL_<UPPER_SNAKE>` env overrides for any field (the env name is
the TOML key path upper-snake-cased: `[server] data_dir` →
`ESCUREL_SERVER_DATA_DIR`). The full table is in
[`../spec/README.md § Configuration`](../spec/README.md#configuration).
Everything below is expressed as env vars so each target is copy-paste
runnable.

---

## Three deploy targets

The M5 acceptance gate is the same binary running in three shapes,
from a laptop up to the substrate. Each target lists the exact
`ESCUREL_*` env set and the command.

### Target A — single binary on a laptop (FS backend, no OTLP)

The inner-loop / demo shape. Local filesystem LaneStore, no
observability backend, no auth issuer (use the test issuer or a local
OIDC stub; for a no-auth smoke run, point at a dev issuer). The model
loads from the HuggingFace cache on first start (the one target where
network egress is acceptable).

```sh
export ESCUREL_SERVER_DATA_DIR=$HOME/.local/share/escurel
export ESCUREL_SERVER_LISTEN_HTTP=127.0.0.1:8080
export ESCUREL_SERVER_LISTEN_GRPC=127.0.0.1:8081

# Filesystem LaneStore — no S3, no spool-to-cloud.
export ESCUREL_STORAGE_BACKEND=fs

# EmbeddingGemma via candle; on a laptop, let it fetch to the HF cache
# under $ESCUREL_SERVER_DATA_DIR/cache/models/ on first start.
export ESCUREL_EMBEDDING_PROVIDER=embeddinggemma
export ESCUREL_EMBEDDING_MODEL=google/embeddinggemma-300m
export ESCUREL_EMBEDDING_DEVICE=cpu
export ESCUREL_EMBEDDING_DIM=768

# Auth: point at whatever issuer you run locally. For test-issuer
# integration runs see crates/escurel-test-support (AuthMode::TestIssuer).
export ESCUREL_AUTH_OIDC_ISSUER=http://127.0.0.1:9000/
export ESCUREL_AUTH_OIDC_AUDIENCE=escurel

# No OTLP: leave ESCUREL_OBSERVABILITY_OTLP_ENDPOINT unset → tracing is
# a no-op. Logs still go to stdout as JSON.
export ESCUREL_OBSERVABILITY_LOG_FORMAT=json

mkdir -p "$ESCUREL_SERVER_DATA_DIR"
escurel-server
```

Drive it with the `escurel` CLI (`ESCUREL_SERVER=http://127.0.0.1:8081`,
`ESCUREL_TOKEN=<test-token>`).

### Target B — systemd unit on a VM (FS, OTLP to local Tempo/Prometheus)

A single VM that you own. FS LaneStore on the VM's disk; traces +
metrics to a co-located OTel collector / Tempo / Prometheus. The model
is staged onto the VM once (no per-start egress).

Stage the model once, then drop the unit below at
`/etc/systemd/system/escurel.service`:

```ini
[Unit]
Description=escurel knowledge-base server
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
User=escurel
Group=escurel
# All config via env; no /etc/escurel/server.toml needed (env overrides
# every field). Put the env in a root-only file.
EnvironmentFile=/etc/escurel/escurel.env
ExecStart=/usr/local/bin/escurel-server
# 12-factor / graceful SIGTERM: flush the S3 spool (n/a on fs), release
# write locks, close DuckDB cleanly.
KillSignal=SIGTERM
TimeoutStopSec=30
Restart=on-failure
RestartSec=5
# Hardening.
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/escurel
StateDirectory=escurel

[Install]
WantedBy=multi-user.target
```

`/etc/escurel/escurel.env` (root-only, `chmod 600`):

```sh
ESCUREL_SERVER_DATA_DIR=/var/lib/escurel
ESCUREL_SERVER_LISTEN_HTTP=0.0.0.0:8080
ESCUREL_SERVER_LISTEN_GRPC=0.0.0.0:8081

ESCUREL_STORAGE_BACKEND=fs

ESCUREL_EMBEDDING_PROVIDER=embeddinggemma
# Staged onto the VM once — absolute local path, loaded via from_local
# (no egress).
ESCUREL_EMBEDDING_MODEL=/opt/escurel/models/embeddinggemma-300m
ESCUREL_EMBEDDING_DEVICE=cpu
ESCUREL_EMBEDDING_DIM=768

ESCUREL_AUTH_OIDC_ISSUER=https://auth.example.com/realms/main
ESCUREL_AUTH_OIDC_AUDIENCE=escurel
ESCUREL_AUTH_TENANT_CLAIM=tenant
ESCUREL_AUTH_ADMIN_ROLE_CLAIM=roles
ESCUREL_AUTH_ADMIN_ROLE_VALUE=escurel:admin

# OTLP to a co-located collector; Prometheus scrapes :9090/metrics.
ESCUREL_OBSERVABILITY_OTLP_ENDPOINT=http://127.0.0.1:4317
ESCUREL_OBSERVABILITY_METRICS_LISTEN=0.0.0.0:9090
ESCUREL_OBSERVABILITY_LOG_FORMAT=json
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now escurel.service
sudo systemctl status escurel.service
journalctl -u escurel.service -f      # JSON lines on stdout
```

### Target C — substrate `nonprod` (S3 LaneStore, OTLP to substrate collector)

The production shape: S3 LaneStore on Hetzner Object Storage, OTLP to
the substrate collector, cattle-node-loss → auto-rebuild from canonical
markdown. This target is **not** a hand-run binary — it is the Nomad
jobspec [`escurel.nomad.hcl`](escurel.nomad.hcl), deployed operator-side
from the substrate repo. The `ESCUREL_*` env set lives in that jobspec;
secrets (S3 keys) come from Vault via the `template` stanza, not env
files.

```sh
# Operator-side, from the substrate repo. Names per substrate.md.
nomad job run \
  -var datacenter=nonprod \
  -var version=<build-version> \
  -var image=<golden-image-ref> \
  -var public_hostname=escurel.nonprod.<domain> \
  -var oidc_issuer=https://oidc.nonprod.<domain>/ \
  -var s3_bucket=datazoo-substrate-app-nonprod \
  -var s3_endpoint=https://nbg1.your-objectstorage.com \
  -var host_volume_name=escurel-tenants \
  docs/deploy/escurel.nomad.hcl
```

Distinctive behaviours of this target (vs A/B):

- **S3 LaneStore.** `ESCUREL_STORAGE_BACKEND=s3`, bucket
  `datazoo-substrate-app-nonprod`, prefix `dz/escurel/lanes/tenants/`.
  The endpoint hostname is used end-to-end (LaneStore + DuckDB httpfs
  secret) per the storage.md hostname-equality constraint.
- **Baked model, no egress.** `ESCUREL_EMBEDDING_MODEL` is the baked
  golden-image path `/opt/escurel/models/embeddinggemma-300m`
  ([`escurel.pkr.hcl`](escurel.pkr.hcl)).
- **OTLP to the substrate collector** at
  `http://otel-collector.service.consul:4317`.
- **Cattle-node-loss → auto-rebuild.** On `/recreate-node` the next
  allocation reconstructs each tenant's `escurel.duckdb` from canonical
  markdown on the LaneStore on first request, with no operator action
  (storage.md HNSW persistence model). This is the M5 substrate
  acceptance gate (substrate.md §10).
- **Backups.** The [`escurel-export-shipper`](escurel-export-shipper.nomad.hcl)
  periodic job ships per-tenant `tenant_export` tarballs to GCS.

### Placeholders you (or the operator) must fill

| Placeholder | Who supplies it | Where |
|---|---|---|
| `<domain>` | operator (DNS) | `public_hostname`, `oidc_issuer` |
| Hetzner OS access/secret key | operator (Vault `kv/data/apps/dz/escurel/<env>/objstore`) | jobspec `template` |
| `host_volume_name` | operator (substrate PR pre-allocates the host volume) | `-var host_volume_name` |
| golden-image ref | operator (Packer build) | `-var image` |
| `escurel-class` node class | operator (substrate `infra/modules/nodes`) | jobspec `constraint` |
| GCS backup bucket + SA key | operator (Vault `.../backups-gcs`) | shipper `template` |
| model-artefact mirror URL | operator (substrate internal store) | `escurel.pkr.hcl` |

None of these are invented here; each resolves to a substrate-repo PR
or a Vault write (see [`substrate.md §9`](substrate.md) dependency
matrix).
