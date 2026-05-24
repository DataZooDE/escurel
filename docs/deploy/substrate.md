# Deployment binding — DataZoo Hetzner agent substrate

**Status:** Binding doc for one deployment target. The core spec
under [`../spec/`](../spec/) is deployment-target-agnostic; this
file names the concrete bindings for the
[`DataZooDE/hetzner-agent-substrate`](https://github.com/DataZooDE/hetzner-agent-substrate)
target. A concrete Nomad jobspec implementing this binding lives
in [`escurel.nomad.hcl`](escurel.nomad.hcl).

New deployment targets (managed-K8s, single-VM for small
operators, etc.) live in sibling `<target>.md` files under
[`../deploy/`](../deploy/); the core spec stays unchanged.

---

## Naming convention — `KB_*` env vars, `escurel-*` substrate names

The binary's runtime config surface (CLI flags, environment
variables, TOML keys) is `KB_*` / `kb.*` — these are the
identifiers the spec README enumerates and the implementation
locks. The substrate-side names (Vault policies, Consul services,
Fabio host tags, Tailscale tags, GCS bucket names, Nomad job
names, placement-group classes) are `escurel-*` / `tag:escurel`
/ `escurel.<env>.<domain>` — these match the repo and product
name. The split is intentional:

- `KB_*` env vars are the *binary's* surface; renaming them
  would break every existing tenant's config and CLI muscle
  memory.
- `escurel-*` substrate names are the *substrate's* surface;
  they appear in operator dashboards, Tailscale ACLs, Fabio
  routing tables, and GCS bucket inventories. Naming them
  after the repo makes those surfaces self-explanatory.

---

## §1 — Identity (OIDC issuer)

| Knob | Value |
|---|---|
| `auth.oidc_issuer` | `https://oidc.<env>.<domain>/` — substrate-provided. Recommended source: Vault OIDC role (substrate already runs Vault per `SPEC.md §2`). Alternative: Dex/Keycloak Nomad job classified as a pet (substrate v2 SPEC delta `Δ-1`) |
| `auth.oidc_audience` | `escurel` |
| `auth.tenant_claim` | `kb_tenant` |
| `auth.admin_role_claim` | `roles` |
| `auth.admin_role_value` | `escurel:admin` |
| `auth.jwks_refresh_secs` | `300` |

Triton — the deployed agent ingress — uses the same issuer.
Escurel and Triton share the OIDC root so principals are
interchangeable across services.

---

## §2 — Storage (LaneStore)

| Knob | Value |
|---|---|
| `storage.backend` | `s3` |
| `storage.s3.bucket` | `escurel-lanes-<env>` (one bucket per env; `prevent_destroy = true` per substrate `SPEC.md §9`) |
| `storage.s3.region` | Hetzner Object Storage region for the single German location (substrate `SPEC.md §2`) |
| `storage.s3.endpoint` | The Hetzner OS hostname for the env. Used end-to-end (LaneStore config + DuckDB `TYPE s3` secret) per the [`storage.md`](../spec/storage.md#the-lanestore-trait) hostname-equality constraint. No `/etc/hosts` rewrites |
| `storage.s3.prefix` | `tenants/` |
| `storage.s3.path_style` | `true` |
| Bucket lifecycle | no expiry on `markdown/` and `kb.duckdb`; `cache/` and `spool/` never exist on S3 |
| Bucket versioning | enabled |

**Operator action required:** bucket provisioning is a
substrate-repo PR. The bucket must be created with
`prevent_destroy = true` and versioning enabled before Escurel
can deploy to the corresponding env.

---

## §3 — Audit collector contract

The substrate ships a Nomad periodic job that tails `kb-server`
allocation stdout, filters by `level ∈ {info, notice, critical}`,
and writes one JSON line per record to the GCS audit bucket
(versioned + retention-locked, `europe-west3`).

The collector relies on the
[`platform.md §Logs`](../spec/platform.md#logs) shape: every line
carries `ts, level, msg, tenant, tool, transport, subject,
trace_id, duration_ms, env`. Ingestion target:
`gs://datazoo-audit-<env>/escurel/<YYYY>/<MM>/<DD>/`.

Triton emits paired audit lines for each call (one at ingress,
one per upstream agent dispatch) carrying the same `trace_id` —
this collector ingests both shapes.

---

## §4 — Backup shipper contract

The substrate ships a tenant-export shipper Nomad periodic job
that calls `KbAdmin.TenantExport` per active tenant on a
configurable cadence, validates the SHA-256 terminator per the
[`protocol.md` tenant_export contract](../spec/protocol.md#tenant_export-as-the-backup-contract-producer),
and uploads each tarball to GCS as a single object.

| Knob | Value |
|---|---|
| Cadence | 1×/24 h per active tenant; per-tenant override via shipper config |
| Target bucket | `gs://datazoode-escurel-backups-<env>/` (versioned + retention-locked) |
| Key format | `<tenant_id>/<YYYY>-<MM>-<DD>T<HH><MM><SS>Z.tar` |
| Retention | per substrate backplane policy (`SPEC.md §5`) — never deleted within the window |
| Audit | one shipment = one audit line with `tool: tenant_export_shipped` |

`kb-server` stays a producer-only; the shipper is the
substrate-side completion of the backup contract.

**Operator action required:** bucket provisioning is a
substrate-repo PR (`prevent_destroy = true`).

---

## §5 — Placement group sizing (`escurel-class`)

Substrate adds a new Nomad client placement group `escurel-class`
(defined in `infra/modules/nodes`):

| Resource | Floor | Notes |
|---|---|---|
| RAM | ≥ 8 GiB | EmbeddingGemma resident (~600 MiB) + per-tenant HNSW working-set (~10 MiB / 1000 blocks) + DuckDB page cache (default 2 GiB) + headroom; scales with active-tenant LRU |
| vCPU | ≥ 4 | HNSW rebuild on cold tenant access and `update_page` embedding are CPU-bound; candle CPU path is the default |
| Local disk | ≥ 50 GiB | spool + embedding cache + transient DuckDB scratch; host-local, not relied on across reschedules per [`storage.md` crash recovery](../spec/storage.md#crash-recovery-summary) |
| Hetzner class | CCX-class or larger | not CX22 |

Spread placement group preserved for HA across hosts.

---

## §6 — Golden image content

The substrate Packer image (`packer/golden.pkr.hcl`) gains:

- candle runtime libs (pure-Rust build — avoids
  `libtorch`/`onnxruntime` dependency)
- EmbeddingGemma model artefact baked at
  `/opt/escurel/models/embeddinggemma-300m/` (~600 MiB)
- DuckDB pinned with `vss` + `fts` extensions pre-loaded
- `kb-server` static Rust binary at `/usr/local/bin/kb-server`

Bake-into-image (not pull-on-start) per the substrate air-gap
defaults — pulling the model at boot needs egress allowance and
breaks `SPEC.md §6` default-deny posture.

---

## §7 — Tailscale tag policy

| Tag | Holder | ACL |
|---|---|---|
| `tag:escurel` | Escurel Nomad allocations | Reaches `tag:srv` for Vault template only; does *not* initiate to `tag:agents` (Escurel is not behind Triton) |
| `tag:ops` | Operators + CI ops identity | May reach Escurel gRPC `:8081` over the tailnet for admin tooling |
| `tag:agents` | Agent pool | Escurel does NOT reach `tag:agents`; only Triton (`tag:cli`) does |

---

## §8 — Ingress (Fabio)

The substrate uses **Fabio** as the ingress router. Escurel
declares public listeners via Consul tags:

```hcl
service { name = "escurel-mcp";  port = "mcp";  tags = ["urlprefix-escurel.<env>.<domain>/mcp"] }
service { name = "escurel-ws";   port = "ws";   tags = ["urlprefix-escurel.<env>.<domain>/ws"]  }
service { name = "escurel-rest"; port = "rest"; tags = ["urlprefix-escurel.<env>.<domain>/"]    }
service { name = "escurel-grpc"; port = "grpc"  /* no urlprefix tag — invisible to Fabio */ }
```

Fabio reads the Consul catalog and materialises one route per
tag. The `escurel-grpc` service stays out of Fabio entirely;
operator tooling reaches it via `escurel-grpc.service.consul`
over the tailnet.

The transport-exposure policy this binding implements is the one
[`protocol.md §Transport-summary`](../spec/protocol.md#transport-summary)
names as the default: MCP/HTTP and WS public via Fabio, gRPC
tailnet-only.

---

## §9 — Substrate dependency matrix

The Escurel binding depends on the following substrate-side
provisions. Until they land, `kb-server` cannot deploy to
substrate `prod`; `nonprod` deployment becomes possible once the
identity, ingress, lanes bucket, and golden-image entries are
merged.

| Substrate change | Escurel binding section |
|---|---|
| Shared OIDC issuer (Vault role or Dex/Keycloak job) | §1 |
| Fabio as ingress router | §8 |
| Audit-log collector → GCS audit bucket | §3 |
| `escurel-lanes-<env>` bucket on Hetzner OS | §2 |
| `datazoode-escurel-backups-<env>` GCS bucket | §4 |
| Tenant-export shipper periodic job | §4 |
| Tailscale `tag:escurel` ACL | §7 |
| EmbeddingGemma + candle baked into golden image | §6 |
| `escurel-class` Nomad client placement group | §5 |
| DR acceptance gate (cattle-node-loss → auto-rebuild) | [`storage.md` HNSW persistence model](../spec/storage.md#hnsw-persistence-model) |

---

## §10 — Acceptance test (per-tenant on substrate `nonprod`)

1. Deploy `kb-server` with one tenant on `nonprod` via
   `/deploy-green`.
2. Create a page via `update_page`; assert canonical markdown
   lands at
   `s3://escurel-lanes-nonprod/tenants/<tenant>/markdown/...`
   on Hetzner OS.
3. Call `tenant_export`; assert the tarball lands in
   `gs://datazoode-escurel-backups-nonprod/<tenant>/<ts>.tar`
   within 60 s.
4. Trigger `/recreate-node` on the `escurel-class` allocation
   host; assert `kb-server` reconstructs the tenant's
   `kb.duckdb` from canonical markdown on first request without
   operator action (per
   [`storage.md` HNSW persistence model](../spec/storage.md#hnsw-persistence-model)).
5. Restore the GCS tarball into a scratch tenant via
   `/restore-dryrun`; assert page count + embedding-search
   recall match the pre-export baseline.
