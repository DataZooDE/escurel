# Deploying escurel

This directory holds the deployment artefacts for `escurel-server`.
The core spec under [`../spec/`](../spec/) is deployment-target-
agnostic; the files here bind it to concrete targets.

| File | What it is |
|---|---|
| [`substrate.md`](substrate.md) | The DataZoo Hetzner substrate binding (Kamal/ghcr/OpenTofu/GCP) — naming, identity, secrets, storage-as-a-pet, exposure, backup. Read this for the substrate target. |
| [`../../Dockerfile`](../../Dockerfile) | The `escurel-server` container image (build → slim runtime). Published to private ghcr by [`../../.github/workflows/publish-image.yml`](../../.github/workflows/publish-image.yml). |
| [`../../deny.toml`](../../deny.toml) | `cargo-deny` config — the machine-enforced license + advisory + source gate. See [§ License + advisory audit](#license--advisory-audit-cargo-deny). |

> **Deployment is the DataZoo substrate (ADR-0013): Kamal on Hetzner cattle
> hosts + OpenTofu + private ghcr + a GCP backplane**, two-actor PR model. The
> per-app **Kamal deploy contract (`kamal/dz-escurel/deploy.yml`) and registry
> row (`apps/registry.yml`) live in the substrate repo**, not here — see
> [`substrate.md`](substrate.md) and the `substrate-platform` skill. The old
> Nomad jobspecs, Packer golden-image fragment, and per-app Tailscale ACL were
> removed (the HashiCorp stack is retired).

> **Two naming surfaces.** The **binary surface** keeps the project
> name: `ESCUREL_*` env vars, `escurel.toml`. The **substrate surface**
> uses the shared `dz-escurel` / `datazoo-substrate-app-<env>/dz/escurel/`
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

Drive it with the `escurel` CLI (`ESCUREL_SERVER=http://127.0.0.1:8080`,
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

### Target C — the DataZoo substrate (Kamal, stateful pet)

The production shape. **Not** a hand-run binary and **not** in this repo's
artefacts: escurel deploys as a Kamal app from the **substrate repo** under the
two-actor model (a PR adds `kamal/dz-escurel/deploy.yml` + an `apps/registry.yml`
row; a GitHub Action runs `kamal deploy`). The full binding — host-1 data
Volume + FsStore, STOP-FIRST deploys, Secret Manager secrets, internal exposure,
restic→GCS Volume backups, cattle-node-loss auto-rebuild — is in
[`substrate.md`](substrate.md). The image is this repo's
[`Dockerfile`](../../Dockerfile) → private ghcr.

Distinctive behaviours of this target (vs A/B):

- **Stateful pet on the host-1 Volume.** `ESCUREL_SERVER_DATA_DIR=/data` bind-
  mounts the Hetzner data Volume; DuckDB + markdown + blobs live there, single-
  writer, **STOP-FIRST** deploys (DuckDB's exclusive lock). (Hetzner Object
  Storage / `STORAGE_BACKEND=s3` is available for blob offload but not required.)
- **Secrets from GCP Secret Manager** → env at deploy (no Vault). Rotation =
  redeploy.
- **Logs → Cloud Logging, `/metrics` → Managed Prometheus** (no per-app
  collector job).
- **Cattle-node-loss → auto-rebuild.** Volume auto-reattaches to a survivor;
  escurel reconstructs `escurel.duckdb` from canonical markdown on first boot,
  no operator action (storage.md crash-recovery; substrate.md §7).
- **Backups** are the substrate's Volume backup (`backup-data.yml`, restic→GCS)
  with a verified `restore-dryrun`; escurel's `tenant_export` remains the
  logical per-tenant export.

## License + advisory audit (`cargo deny`)

The dependency tree is gated by [`cargo-deny`](https://embarkstudios.github.io/cargo-deny/)
against the root [`deny.toml`](../../deny.toml). The allow-list there is
the machine-enforced form of [`../spec/roadmap.md § Licenses`](../spec/roadmap.md#licenses)
("Permissive across the board. No GPL surface."). M5 re-runs this audit and
freezes the dep set.

```sh
# One-time: install the tool.
cargo install cargo-deny --locked

# Run all four checks (licenses, advisories, bans, sources).
cargo deny check

# Or scope to one section while iterating.
cargo deny check licenses
cargo deny check advisories
```

What each section enforces:

- **`licenses`** — every crate's SPDX license must be on the permissive
  allow-list (MIT / Apache-2.0 / BSD / ISC / Unicode / Zlib / MPL-2.0,
  plus the workspace's own `BUSL-1.1`). A new dep with a copyleft or
  unknown license fails here.
- **`advisories`** — checks the RustSec DB; **yanked crates are denied**.
  `unmaintained` advisories are scoped to direct workspace deps
  (`unmaintained = "workspace"`) so deep-transitive maintenance noise
  doesn't block. Four advisories are explicitly ignored with dated
  rationales in `deny.toml` (see the `[advisories].ignore` comments): the
  three `rustls-webpki 0.101.7` issues (RUSTSEC-2026-0098/0099/0104) sit on
  the legacy `rustls 0.21` feature path that escurel never compiles — the
  shipped binary uses the patched `rustls 0.23` / `webpki 0.103.13` — and
  the rsa Marvin sidechannel (RUSTSEC-2023-0071) is dev-dependency-only.
  None has an in-semver upgrade path. A *new* vuln with a fix should be
  bumped, not ignored.
- **`bans`** — duplicate versions **warn** (native deps pull in dupes);
  wildcard (`*`) version requirements **warn** today. The only wildcards in
  the tree are the workspace's own intra-workspace path deps; there are
  zero external wildcards. To flip this back to `deny` (the intended
  setting), mark each workspace member `publish = false` so
  `allow-wildcard-paths` exempts the path deps — tracked as a follow-up.
- **`sources`** — only crates.io; unknown registries and git deps are
  **denied**.

Deps are frozen via the committed `Cargo.lock` (a locked decision in
`CLAUDE.md`). Do not run a blanket `cargo update`; if the audit forces a
specific bump, change that one crate in `Cargo.toml` and `cargo update -p
<crate>` only.

## Placeholders the operator supplies (all via substrate-repo PRs)

| Placeholder | Who supplies it | Where |
|---|---|---|
| `<env>` / internal hostname (`dz-escurel.<env>.int`) | operator (substrate DNS) | `apps/registry.yml` exposure |
| image tag (`ghcr.io/datazoode/escurel-server:<tag>`) | this repo's `publish-image.yml` build | `apps/registry.yml` / `deploy.yml` |
| `ghcr-pull-token` | operator (Secret Manager) | substrate host pull auth |
| `/data` Volume subdir on host-1 | operator (substrate PR) | `kamal/dz-escurel/deploy.yml` `volumes:` |
| Gemini key / webhook secret / (optional) S3 keys | operator (Secret Manager) | `deploy.yml` `env.secret` |
| OIDC issuer(s) + audience | operator (substrate auth) | `deploy.yml` `env.clear` |

None of these are invented here; each resolves to a **substrate-repo PR** (a
deploy/registry change or a Secret Manager entry) — see
[`substrate.md §6`](substrate.md) dependency matrix and the `substrate-platform`
skill.
