// datazoode-escurel — single-replica stateful service on the DataZoo
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
// cattle (rebuildable from `s3://escurel-lanes-<env>/`); the
// allocation is pet (one replica per env, host-volume-pinned for
// warm-cache performance). On `/recreate-node` the next allocation
// rebuilds from S3 — see `docs/spec/storage.md` HNSW persistence
// model.
//
// Naming: `escurel-*` / `ESCUREL_*` everywhere — substrate surface
// (Consul services, Fabio tags, Tailscale tag, Vault policy, GCS
// bucket) and binary surface (env vars, TOML, CLI) share the same
// project name. See `docs/deploy/substrate.md §Naming convention`.

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
  description = "Hetzner Object Storage bucket for canonical markdown + escurel.duckdb. Convention: escurel-lanes-<env>. Per docs/deploy/substrate.md §2."
  // Example: "escurel-lanes-nonprod"
}

variable "s3_endpoint" {
  type        = string
  description = "Hetzner OS endpoint hostname. Used end-to-end (LaneStore config + DuckDB httpfs secret) per the hostname-equality constraint in docs/spec/storage.md."
}

job "datazoode-escurel" {
  type        = "service"
  datacenters = [var.datacenter]

  update {
    canary            = 0          // no canary — only one replica
    auto_promote      = false
    auto_revert       = true
    max_parallel      = 1
    min_healthy_time  = "30s"
    healthy_deadline  = "5m"
    progress_deadline = "10m"
  }

  group "escurel" {
    count = 1

    // Pin to the node that hosts the volume. The substrate operator
    // pre-allocates this host volume via the substrate repo.
    volume "tenants" {
      type      = "host"
      source    = var.host_volume_name
      read_only = false
    }

    // Four named ports, one per transport. Fabio routes the first
    // three by `urlprefix-` tag; gRPC is tailnet-only (no Fabio tag).
    network {
      mode = "bridge"
      port "mcp"  { to = 8080 }     // MCP-over-HTTP
      port "ws"   { to = 8080 }     // WebSocket on the same listener
      port "rest" { to = 8080 }     // /healthz, /readyz, /metrics
      port "grpc" { to = 8081 }     // gRPC admin surface
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

    // -------- gRPC (tailnet-only; no urlprefix-* tag) --------
    service {
      name     = "escurel-grpc"
      port     = "grpc"
      provider = "consul"

      check {
        type     = "tcp"
        interval = "15s"
        timeout  = "3s"
      }
    }

    task "escurel-server" {
      driver = "docker"

      // Vault policy `apps-escurel` grants read access to the OIDC
      // signing key, the S3 access key for the lanes bucket, and the
      // (optional) Gemini embeddings API key. Policy lives in the
      // substrate repo; rotation is operator-controlled.
      vault {
        policies = ["apps-escurel"]
      }

      volume_mount {
        volume      = "tenants"
        destination = "/data"
        read_only   = false
      }

      config {
        image = var.image
        ports = ["mcp", "ws", "rest", "grpc"]
      }

      // Vault-templated secrets land in /secrets and are sourced by
      // the binary at startup. ESCUREL_* envs are the binary's locked
      // config surface (see docs/spec/README.md § Configuration).
      template {
        destination = "secrets/s3.env"
        env         = true
        data        = <<EOH
ESCUREL_STORAGE_S3_ACCESS_KEY_ID={{ with secret "kv/data/apps/escurel/s3" }}{{ .Data.data.access_key_id }}{{ end }}
ESCUREL_STORAGE_S3_SECRET_ACCESS_KEY={{ with secret "kv/data/apps/escurel/s3" }}{{ .Data.data.secret_access_key }}{{ end }}
EOH
      }

      env {
        APP      = "datazoode-escurel"
        ENV      = "${var.datacenter}"
        VERSION  = "${var.version}"

        // Data + cache dirs live on the pinned host volume.
        ESCUREL_SERVER_DATA_DIR = "/data"
        ESCUREL_CONFIG          = "/local/server.toml"

        // Listen addresses. MCP/WS/REST share the HTTP listener.
        ESCUREL_SERVER_LISTEN_HTTP = "0.0.0.0:8080"
        ESCUREL_SERVER_LISTEN_GRPC = "0.0.0.0:8081"

        // Auth — substrate Vault OIDC role.
        ESCUREL_AUTH_OIDC_ISSUER      = "${var.oidc_issuer}"
        ESCUREL_AUTH_OIDC_AUDIENCE    = "escurel"
        ESCUREL_AUTH_TENANT_CLAIM     = "escurel_tenant"
        ESCUREL_AUTH_ADMIN_ROLE_CLAIM = "roles"
        ESCUREL_AUTH_ADMIN_ROLE_VALUE = "escurel:admin"
        ESCUREL_AUTH_JWKS_REFRESH_SECS = "300"

        // Storage — Hetzner Object Storage. Endpoint hostname MUST
        // match between LaneStore and DuckDB httpfs secret per
        // docs/spec/storage.md hostname-equality constraint.
        ESCUREL_STORAGE_BACKEND       = "s3"
        ESCUREL_STORAGE_S3_BUCKET     = "${var.s3_bucket}"
        ESCUREL_STORAGE_S3_ENDPOINT   = "${var.s3_endpoint}"
        ESCUREL_STORAGE_S3_PREFIX     = "tenants/"
        ESCUREL_STORAGE_S3_PATH_STYLE = "true"

        // Embedding — EmbeddingGemma baked into the golden image at
        // /opt/escurel/models/embeddinggemma-300m/ (per
        // docs/deploy/substrate.md §6).
        ESCUREL_EMBEDDING_PROVIDER = "embeddinggemma"
        ESCUREL_EMBEDDING_MODEL    = "google/embeddinggemma-300m"
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
        cpu    = 4000   // MHz; ≥ 4 vCPU
        memory = 8192   // MiB; ≥ 8 GiB
      }
    }
  }
}
