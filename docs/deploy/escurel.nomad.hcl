// dz-escurel — single-replica stateful service on the DataZoo
// substrate.
//
// ⚠️  PET WARNING — READ BEFORE FORKING ⚠️
//
// The substrate is designed for cattle workloads. A "stateful service"
// here means: one replica, pinned to a host volume, no canary, no
// blue/green. This is operationally heavier than every other template
// in the substrate-platform skill. **Talk to an operator before
// deploying one.**
//
// Escurel is a stateful service because each tenant's per-node DuckDB
// file (`escurel.duckdb`) is rebuilt from canonical markdown on the
// LaneStore on first request after a node loss. The DuckDB file is
// cattle (rebuildable from `s3://datazoo-substrate-app-<env>/dz/escurel/lanes/`); the
// allocation is pet (one replica per env, host-volume-pinned for
// warm-cache performance). On `/recreate-node` the next allocation
// rebuilds from S3 — see `docs/spec/storage.md` HNSW persistence
// model.
//
// Naming: substrate surface uses the substrate-platform skill's
// shared convention — `dz-escurel` (Nomad job + Consul service),
// `apps-dz` (Vault policy), `datazoo-substrate-app-<env>/dz/escurel/`
// (S3 + GCS prefix under the shared substrate bucket). The binary
// surface keeps `ESCUREL_*` envs and `escurel.toml` config — those
// are the application's identity, not the substrate's. See
// `docs/deploy/substrate.md §Naming convention`.

variable "datacenter" {
  type        = string
  description = "Nomad datacenter (env): nonprod | prod."
}

variable "version" {
  type        = string
  description = "Build version (e.g. v1.0.0)."
}

variable "image" {
  type        = string
  description = "Container image, pinned by SHA or immutable tag. Default golden image bakes escurel-server + EmbeddingGemma."
  // Example: "registry.datazoo.internal/escurel:v1.0.0@sha256:..."
}

variable "host_volume_name" {
  type        = string
  description = "Nomad host volume name for the per-node DuckDB lane store. Allocated by the operator via a substrate PR; do NOT invent one."
  default     = "escurel-tenants"
}

variable "public_hostname" {
  type        = string
  description = "Public hostname for Fabio routing. Convention: escurel.<env>.<domain>."
  // Example: "escurel.nonprod.datazoo.cloud"
}

variable "oidc_issuer" {
  type        = string
  description = "OIDC issuer URL (substrate Vault role or Dex/Keycloak job). Per docs/deploy/substrate.md §1."
}

variable "s3_bucket" {
  type        = string
  description = "Hetzner Object Storage bucket — substrate-shared `datazoo-substrate-app-<env>`, with escurel's content under the prefix `dz/escurel/lanes/`. Per docs/deploy/substrate.md §2."
  // Example: "datazoo-substrate-app-nonprod"
}

variable "s3_endpoint" {
  type        = string
  description = "Hetzner OS endpoint hostname. Used end-to-end (LaneStore config + DuckDB httpfs secret) per the hostname-equality constraint in docs/spec/storage.md."
}

job "dz-escurel" {
  type        = "service"
  datacenters = [var.datacenter]

  update {
    canary            = 0 // no canary — only one replica
    auto_promote      = false
    auto_revert       = true
    max_parallel      = 1
    min_healthy_time  = "30s"
    healthy_deadline  = "5m"
    progress_deadline = "10m"
  }

  group "escurel" {
    count = 1

    // Pin to the `escurel-class` placement group (substrate adds this
    // Nomad client class in `infra/modules/nodes` per
    // docs/deploy/substrate.md §5: ≥ 8 GiB RAM, ≥ 4 vCPU, CCX-class).
    // Without it Nomad may place the alloc on a default cli node that
    // cannot hold EmbeddingGemma + the per-tenant HNSW working set.
    constraint {
      attribute = "${node.class}"
      value     = "escurel-class"
    }

    // Spread allocations across distinct hosts where the env has the
    // capacity (substrate.md §5: "Spread placement preserved for HA").
    // On the 2-cli nonprod/prod clusters this is a soft preference; it
    // costs nothing here because escurel is count = 1.
    spread {
      attribute = "${node.unique.id}"
    }

    // Pin to the node that hosts the volume. The substrate operator
    // pre-allocates this host volume via the substrate repo. NOTE:
    // the substrate-platform skill now treats CSI (Hetzner Cloud
    // Volumes via `csi.hetzner.cloud`) as the PRIMARY stateful
    // template (skill v2.2.0 / ADR-0003) and deprecates host_volume.
    // Escurel stays on host_volume deliberately for warm-cache
    // locality of `escurel.duckdb`; the DuckDB file is rebuildable
    // from the LaneStore so the durability the CSI detach/reattach
    // buys is not load-bearing here. See substrate skill ref 12 for
    // the migration path if this trade-off changes.
    volume "tenants" {
      type      = "host"
      source    = var.host_volume_name
      read_only = false
    }

    // Named ports. The HTTP listener (8080) carries MCP, WS, REST,
    // /healthz, /readyz; Fabio routes mcp/ws/rest by `urlprefix-` tag.
    // metrics (9090) is tailnet-only (no Fabio tag).
    network {
      mode = "bridge"
      port "mcp" { to = 8080 }     // MCP-over-HTTP
      port "ws" { to = 8080 }      // WebSocket on the same listener
      port "rest" { to = 8080 }    // /healthz, /readyz
      port "metrics" { to = 9090 } // Prometheus /metrics
    }

    // -------- MCP-over-HTTP (public via Fabio) --------
    service {
      name     = "escurel-mcp"
      port     = "mcp"
      provider = "consul"
      tags     = ["urlprefix-${var.public_hostname}/mcp"]

      // /healthz is liveness only. /readyz (used by canary promotion)
      // returns 200 only when embedding is loaded, LaneStore is
      // reachable, and the per-tenant DuckDB write lock can be acquired.
      check {
        type     = "http"
        path     = "/healthz"
        interval = "15s"
        timeout  = "3s"
      }
    }

    // -------- WebSocket (public via Fabio) --------
    service {
      name     = "escurel-ws"
      port     = "ws"
      provider = "consul"
      tags     = ["urlprefix-${var.public_hostname}/ws"]

      check {
        type     = "http"
        path     = "/healthz"
        interval = "15s"
        timeout  = "3s"
      }
    }

    // -------- REST (public via Fabio: /healthz, /readyz, /metrics) --------
    service {
      name     = "escurel-rest"
      port     = "rest"
      provider = "consul"
      tags     = ["urlprefix-${var.public_hostname}/"]

      check {
        type     = "http"
        path     = "/readyz"
        interval = "30s"
        timeout  = "5s"
      }
    }

    // -------- Prometheus /metrics (tailnet-only; no Fabio tag) --------
    // The `metrics`-prefixed tag is the convention the substrate's
    // (Phase 6+) Prometheus scraper keys off; until it ships this is a
    // no-op (substrate skill ref 07). Scraped over the tailnet at
    // escurel-metrics.service.consul:<port>/metrics.
    service {
      name     = "escurel-metrics"
      port     = "metrics"
      provider = "consul"
      tags     = ["metrics", "metrics-path-/metrics"]

      check {
        type     = "http"
        path     = "/metrics"
        interval = "30s"
        timeout  = "5s"
      }
    }

    task "escurel-server" {
      driver = "docker"

      // Graceful shutdown: on a `/recreate-node` drain (or a normal
      // deploy) Nomad sends SIGTERM, then SIGKILL after kill_timeout.
      // escurel-server flushes the per-tenant S3 spool, releases the
      // per-tenant write locks, and closes DuckDB cleanly inside this
      // window. 12-factor principle 3 (graceful SIGTERM) +
      // CLAUDE.md §4.
      kill_signal  = "SIGTERM"
      kill_timeout = "30s"

      // Vault policy `apps-dz` grants read access to the OIDC
      // signing key, the S3 access key for the lanes bucket, and the
      // (optional) Gemini embeddings API key. Policy lives in the
      // substrate repo; rotation is operator-controlled. The role
      // accepts any `dz-*` job id, so dz-escurel → apps-dz (substrate
      // skill ref 02).
      vault {
        policies = ["apps-dz"]
      }

      volume_mount {
        volume      = "tenants"
        destination = "/data"
        read_only   = false
      }

      config {
        image = var.image
        ports = ["mcp", "ws", "rest", "metrics"]
      }

      // Vault-templated secrets land in /secrets and are sourced by
      // the binary at startup. ESCUREL_* envs are the binary's locked
      // config surface (see docs/spec/README.md § Configuration).
      //
      // Path convention is `kv/data/apps/<co>/<app>/<env>/<leaf>`
      // (substrate skill refs 02 + 04); `{{ env "NOMAD_DC" }}` resolves
      // to nonprod|prod at render time so one HCL serves both envs. The
      // operator writes the secret value (key shape: access_key_id /
      // secret_access_key); rotation re-renders + restarts the task.
      template {
        destination = "secrets/s3.env"
        env         = true
        change_mode = "restart"
        data        = <<EOH
{{ with secret (printf "kv/data/apps/dz/escurel/%s/objstore" (env "NOMAD_DC")) -}}
ESCUREL_STORAGE_S3_ACCESS_KEY_ID={{ .Data.data.access_key_id }}
ESCUREL_STORAGE_S3_SECRET_ACCESS_KEY={{ .Data.data.secret_access_key }}
{{- end }}
EOH
      }

      // Minimal base config file. Every value here is also pinned via
      // ESCUREL_* env below (which override any TOML field per
      // docs/spec/README.md § Configuration); this file exists so the
      // binary's `${ESCUREL_CONFIG}` load has a real target rather than
      // relying on the env-only path. The sizing knobs live here so
      // capacity planning is one place (spec README sizing table).
      template {
        destination = "local/server.toml"
        change_mode = "restart"
        data        = <<EOH
[server]
data_dir = "/data"

[storage]
backend = "s3"

[embedding]
provider = "embeddinggemma"
device   = "cpu"
dim      = 768

[concurrency]
tenant_lru_cap        = 64
duckdb_read_pool      = 16
embed_pool            = 32
write_lock_timeout_ms = 5000
EOH
      }

      env {
        APP     = "dz-escurel"
        ENV     = "${var.datacenter}"
        VERSION = "${var.version}"

        // Data + cache dirs live on the pinned host volume.
        ESCUREL_SERVER_DATA_DIR = "/data"
        ESCUREL_CONFIG          = "/local/server.toml"

        // Listen addresses. MCP/WS/REST share the HTTP listener.
        ESCUREL_SERVER_LISTEN_HTTP = "0.0.0.0:8080"

        // Auth — substrate Vault OIDC role.
        ESCUREL_AUTH_OIDC_ISSUER       = "${var.oidc_issuer}"
        ESCUREL_AUTH_OIDC_AUDIENCE     = "escurel"
        ESCUREL_AUTH_TENANT_CLAIM      = "escurel_tenant"
        ESCUREL_AUTH_ADMIN_ROLE_CLAIM  = "roles"
        ESCUREL_AUTH_ADMIN_ROLE_VALUE  = "escurel:admin"
        ESCUREL_AUTH_JWKS_REFRESH_SECS = "300"

        // Storage — Hetzner Object Storage. Endpoint hostname MUST
        // match between LaneStore and DuckDB httpfs secret per
        // docs/spec/storage.md hostname-equality constraint.
        ESCUREL_STORAGE_BACKEND     = "s3"
        ESCUREL_STORAGE_S3_BUCKET   = "${var.s3_bucket}"
        ESCUREL_STORAGE_S3_ENDPOINT = "${var.s3_endpoint}"
        // Escurel's content sits under the substrate-shared bucket
        // at `dz/escurel/lanes/` (per docs/deploy/substrate.md §2's
        // naming convention). `tenants/` is the per-app subkey
        // inside that, so the full path is
        // `s3://<bucket>/dz/escurel/lanes/tenants/<tenant>/...`.
        ESCUREL_STORAGE_S3_PREFIX     = "dz/escurel/lanes/tenants/"
        ESCUREL_STORAGE_S3_PATH_STYLE = "true"

        // Embedding — EmbeddingGemma baked into the golden image at
        // /opt/escurel/models/embeddinggemma-300m/ (per
        // docs/deploy/substrate.md §6 + escurel.pkr.hcl). ESCUREL_EMBEDDING_MODEL
        // is an ABSOLUTE LOCAL PATH on substrate, not a HF repo id —
        // the binary loads via CandleEmbedder::from_local so no network
        // egress happens at runtime (air-gap default, substrate SPEC §6).
        // The directory holds config.json + tokenizer.json + model.safetensors.
        ESCUREL_EMBEDDING_PROVIDER = "embeddinggemma"
        ESCUREL_EMBEDDING_MODEL    = "/opt/escurel/models/embeddinggemma-300m"
        ESCUREL_EMBEDDING_DEVICE   = "cpu"
        ESCUREL_EMBEDDING_DIM      = "768"

        // Observability — OTLP to substrate collector; Prometheus
        // scrape on :9090.
        ESCUREL_OBSERVABILITY_OTLP_ENDPOINT  = "http://otel-collector.service.consul:4317"
        ESCUREL_OBSERVABILITY_METRICS_LISTEN = "0.0.0.0:9090"
        ESCUREL_OBSERVABILITY_LOG_FORMAT     = "json"
      }

      // Substrate `escurel-class` floors per docs/deploy/substrate.md §5.
      // CPU is MHz; the candle CPU path is the dominant consumer during
      // index rebuild and embedding.
      resources {
        cpu    = 4000 // MHz; ≥ 4 vCPU
        memory = 8192 // MiB; ≥ 8 GiB
      }
    }
  }
}
