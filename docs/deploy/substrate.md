# Deployment binding — DataZoo Hetzner substrate

**Status:** Binding doc for one deployment target. The core spec under
[`../spec/`](../spec/) is deployment-target-agnostic; this file names the
concrete bindings for the
[`DataZooDE/hetzner-agent-substrate`](https://github.com/DataZooDE/hetzner-agent-substrate)
target.

The substrate is **Kamal on 3 Hetzner cattle hosts + OpenTofu (CI-applied) +
private ghcr.io + a GCP backplane** (Secret Manager, GCS, Cloud Logging, Managed
Prometheus), operated under a **two-actor model**: changes are PRs against the
substrate repo; a GitHub Action is the sole mutator. For the platform mechanics
read the **`substrate-platform`** skill (`references/01-deploy-an-app`,
`04-stateful-and-storage`, `03-secrets`, `02-exposure`, `05-observability`,
`07-operating`). This doc only names escurel's bindings.

> **The HashiCorp stack (Nomad / Consul / Vault / Fabio) and the Packer golden
> image are retired** (archived on the substrate's `hashicorp` branch). Earlier
> revisions of this doc described them; they no longer apply.

---

## What escurel is on the substrate

escurel-server is a **stateful pet** — a single-writer DuckDB + a canonical
markdown corpus + content-addressed blobs, all on one host's disk. So it follows
the substrate's single-writer-stateful pattern
(`substrate-platform/references/04`):

- **Pinned to the data-primary host** (`servers.web.hosts: [<env>-host-1]`),
  bind-mounting a subdir of the host-1 **Hetzner data Volume** at `/data`
  (`ESCUREL_SERVER_DATA_DIR=/data`). The Volume `prevent_destroy`s and
  auto-reattaches to a survivor on host loss.
- **STOP-FIRST deploys.** DuckDB takes an exclusive file lock, so blue/green
  (old + new container both bind-mounting the Volume) would deadlock. The
  `kamal/dz-escurel/deploy.yml` uses the `STOP_FIRST` path (`kamal app stop`
  then deploy) — a brief deploy-time gap, acceptable for a single-host pet.
  (escurel is the substrate skill's canonical example of this.)
- **Default store is the Volume (FsStore).** Markdown, `escurel.duckdb`, and
  `blobs/` live on the Volume; backups are the substrate's Volume backup
  (below), not an app-level shipper. Hetzner Object Storage (S3) remains
  available for blob offload (`ESCUREL_STORAGE_BACKEND=s3`) but is not required.

## Image + deploy

| | |
|---|---|
| Image | `ghcr.io/datazoode/escurel-server`, built by **this repo's** [`Dockerfile`](../../Dockerfile) + [`.github/workflows/publish-image.yml`](../../.github/workflows/publish-image.yml) (an *external build* registry entry — Kamal pulls, doesn't build) |
| Deploy contract | `kamal/dz-escurel/deploy.yml` in the **substrate repo** (fork `substrate-platform/templates/kamal-deploy.yml`); `kamal deploy` runs on merge |
| Registry row | `apps/registry.yml` in the substrate repo (fork `templates/registry-entry.yml`): `image`, `build: external`, `exposure`, `port: 8080`, `kamal`, `secrets` |
| Health | `GET /healthz` (liveness, dependency-free), `GET /readyz` (readiness), `GET /version`, `GET /metrics` — the substrate health contract (`references/05`) |
| `LABEL service=dz-escurel` | required on the image for kamal-proxy routing (see [`2026-06-13-kamal-service-label`](../notes/discovered/2026-06-13-kamal-service-label.md)) |

## Naming convention

Two surfaces, both load-bearing.

**Binary surface — `ESCUREL_*` / `escurel.*`.** Runtime config (env vars, TOML
keys) keeps the project name; these are what the spec locks and don't change
with the substrate.

**Substrate surface — the substrate-platform shared convention**, so substrate
tooling sees one product:

| Substrate surface | Value |
|---|---|
| Kamal app / registry id / service label | `dz-escurel` |
| Secrets | GCP **Secret Manager**, fetched at deploy → container env (no Vault) |
| Tailscale tag | `tag:dz-escurel` (substrate-managed ACLs; no per-app ACL file) |
| Internal hostname | `dz-escurel.<env>.int` (tailnet wildcard) |
| S3 prefix (Hetzner OS, if used) | `datazoo-substrate-app-<env>/dz/escurel/lanes/` |
| GCS prefix (Volume backups) | `…/dz/escurel/` under the substrate backup bucket |

## §1 — Identity (OIDC)

escurel shares the substrate OIDC root with Triton (and Carl, and the
escurel-explore BFF) — principals are interchangeable. The trust set is the
`ESCUREL_AUTH_OIDC_ISSUER` (+ `_2`, `_3`, …) chain (see
[`escurel-explore-live-bff`] usage). Secrets/keys come from Secret Manager.

| Knob | Value |
|---|---|
| `auth.oidc_issuer` (+ `_2`/`_3`) | the substrate's issuers (Triton primary; Carl; the explore BFF) |
| `auth.oidc_audience` | `escurel` |
| `auth.tenant_claim` | `escurel_tenant` |
| `auth.admin_role_claim` / `_value` | `roles` / `escurel:admin` |

[`escurel-explore-live-bff`]: ../notes/

## §2 — Secrets (GCP Secret Manager)

No Vault. Each secret is a Secret Manager entry fetched at deploy into the
container env (`references/03`); rotation = redeploy. escurel's secret-bearing
env:

| Secret | Env var |
|---|---|
| Gemini API key (when `EMBEDDING_PROVIDER=gemini`) | `ESCUREL_GEMINI_API_KEY` |
| Capture-webhook HMAC secret | `ESCUREL_WEBHOOK_SECRET` |
| S3 keys (only if `STORAGE_BACKEND=s3`) | `ESCUREL_STORAGE_S3_ACCESS_KEY_ID` / `_SECRET_ACCESS_KEY` |

Air-gap note: an air-gapped env sets `ESCUREL_EMBEDDING_PROVIDER=embeddinggemma`
(local weights, no egress) or `zero`; only the `gemini` provider needs a key +
cloud egress.

## §3 — Observability (GCP backplane)

Structured JSON logs on **stdout** → **Cloud Logging** (no per-app collector
job). `/metrics` (Prometheus exposition on the dedicated listener) → **Managed
Prometheus**. Per-record audit fields (`tenant`, `tool`, `subject`, `trace_id`,
`duration_ms`) ride the log lines (`platform.md §Logs`).

## §4 — Backups + DR (the data Volume)

Backups are the substrate's **Volume** backup, not an app shipper: the
substrate `backup-data.yml` (6h cron) rsyncs the Volume over Tailscale SSH and
runs **restic → GCS**; `-f mode=restore-dryrun` is the verified restore path
(`references/04`, `07`). escurel's own `tenant_export` MCP tool remains the
*logical* per-tenant export, but durable DR is the Volume snapshot.

Cattle-node-loss DR is intrinsic: on a fresh host the Volume reattaches and
escurel rebuilds `escurel.duckdb` from the canonical markdown on first boot
(`storage.md` crash-recovery).

## §5 — Exposure

The required `internal | external` choice lives in `apps/registry.yml`
(`references/02`; always ask, fail-closed). escurel's MCP/HTTP + WS surface on
`:8080` is typically **internal** (tailnet-only via `dz-escurel.<env>.int`),
reached by Triton/Carl/the BFF and by operators for the admin-role-gated MCP
tools. Public exposure (Hetzner LB + Let's Encrypt) only if a browser client
talks to it directly.

## §6 — Substrate dependency matrix

| Substrate-repo change | escurel binding |
|---|---|
| `kamal/dz-escurel/deploy.yml` (host-1 pin, STOP_FIRST, `/data` Volume mount) | host pin + store |
| `apps/registry.yml` row (external build, exposure, port, secrets) | image + exposure |
| Secret Manager entries (`gemini-api-key`, `webhook-secret`, optional S3 keys) | §2 |
| `ghcr-pull-token` for the private image | image |
| Tailscale `tag:dz-escurel` ACL + `dz-escurel.<env>.int` DNS | §5 |
| Volume subdir for `/data` + inclusion in `backup-data.yml` | store + §4 |

## §7 — Acceptance (substrate `nonprod`)

1. Merge the registry row + `kamal/dz-escurel/deploy.yml`; the deploy Action
   runs `kamal deploy` (STOP-FIRST) green across host-1.
2. `update_page` a page; assert canonical markdown lands under `/data` on the
   host-1 Volume.
3. Run `backup-data.yml`; then `-f mode=restore-dryrun` and assert the snapshot
   restores + `restic check` passes.
4. Recreate host-1 (`replace_targets`); assert the Volume reattaches and escurel
   rebuilds `escurel.duckdb` from markdown on first request, no operator action.
