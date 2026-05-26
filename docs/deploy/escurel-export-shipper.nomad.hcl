// dz-escurel-export-shipper — periodic tenant-export shipper.
//
// The substrate-side completion of escurel's backup contract
// (docs/deploy/substrate.md §4). `escurel-server` is a producer-only:
// it can emit a `tenant_export` tarball via the admin gRPC surface
// (EscurelAdmin.TenantExport) but never ships it anywhere. This
// periodic batch job is the shipper: once per cadence it enumerates
// active tenants, calls TenantExport per tenant, validates the
// SHA-256 terminator per the protocol.md tenant_export contract, and
// uploads each tarball to the substrate GCS backup bucket as one
// object.
//
// Forked from substrate-platform skill `batch-job.nomad.hcl` +
// the periodic pattern in skill ref 13. Naming uses the post-M5
// `dz-` prefix (substrate.md § Naming convention).
//
// WHY GCS (not Hetzner OS): per skill ref 04 + substrate glossary,
// recovery/integrity-critical data (backups, audit, TF state) lives
// in GCS `europe-west3` (versioned + retention-locked); app/customer
// data lives in Hetzner OS. tenant_export tarballs are DR artefacts,
// so they go to GCS — same split substrate.md §4 names.
//
// Deploy (operator-side, from the substrate repo):
//   nomad job run -var datacenter=nonprod -var version=<ver> \
//     -var image=<shipper-image> docs/deploy/escurel-export-shipper.nomad.hcl

variable "datacenter" {
  type        = string
  description = "Nomad datacenter (env): nonprod | prod."
}

variable "version" {
  type        = string
  description = "Build version of the shipper image."
}

variable "image" {
  type        = string
  description = "Shipper container image, pinned by digest. Bundles the escurel admin client (gRPC) + a GCS uploader (gsutil/rclone). NEVER :latest."
  // Example: "registry.datazoo.internal/escurel-export-shipper@sha256:..."
}

variable "cadence_cron" {
  type        = string
  default     = "0 2 * * *"
  description = "Default 1x/24h at 02:00 UTC. substrate.md §4: 1x/24h per active tenant, per-tenant override via shipper config. Staggered away from the substrate's own backup crons (skill ref 13)."
}

job "dz-escurel-export-shipper" {
  type        = "batch"
  datacenters = [var.datacenter]

  periodic {
    crons            = [var.cadence_cron]
    prohibit_overlap = true
    time_zone        = "UTC"
  }

  group "ship" {
    count = 1

    // The shipper reaches escurel's admin surface over the tailnet
    // (escurel-grpc.service.consul:8081) — it is NOT a public client.
    // It runs on a default cli node; it does not need the
    // escurel-class placement floor (it is I/O-bound, not embedding-bound).

    // Batch reschedule: one retry; a transient GCS/network blip on the
    // next tick is preferable to a hot retry loop (skill ref 13).
    reschedule {
      attempts  = 1
      interval  = "24h"
      delay     = "5m"
      unlimited = false
    }

    task "ship" {
      driver = "docker"

      // apps-dz grants read on kv/data/apps/dz/escurel/<env>/* — the
      // admin OIDC token (for the gRPC call) and the GCS uploader
      // credentials both live there (skill ref 02).
      vault {
        policies = ["apps-dz"]
      }

      config {
        image = var.image
      }

      // Admin bearer + GCS creds, both from Vault. `escurel:admin`
      // role in the OIDC token lets EscurelAdmin.TenantExport through
      // (substrate.md §1 admin_role_value). The GCS service-account
      // key is scoped write-only to the escurel backups prefix.
      template {
        destination = "secrets/shipper.env"
        env         = true
        change_mode = "restart"
        data        = <<EOH
{{ with secret (printf "kv/data/apps/dz/escurel/%s/admin" (env "NOMAD_DC")) -}}
ESCUREL_TOKEN={{ .Data.data.admin_bearer }}
{{- end }}
{{ with secret (printf "kv/data/apps/dz/escurel/%s/backups-gcs" (env "NOMAD_DC")) -}}
GCS_BACKUP_BUCKET={{ .Data.data.bucket }}
GCS_BACKUP_PREFIX={{ .Data.data.prefix }}
{{- end }}
EOH
      }

      // The GCS service-account JSON lands as a file (the uploader
      // reads GOOGLE_APPLICATION_CREDENTIALS).
      template {
        destination = "secrets/gcs-sa.json"
        change_mode = "restart"
        data        = <<EOH
{{ with secret (printf "kv/data/apps/dz/escurel/%s/backups-gcs" (env "NOMAD_DC")) }}{{ .Data.data.service_account_json }}{{ end }}
EOH
      }

      env {
        APP     = "dz-escurel-export-shipper"
        ENV     = "${var.datacenter}"
        VERSION = "${var.version}"

        // Admin gRPC endpoint over the tailnet (the escurel-grpc
        // Consul service from escurel.nomad.hcl).
        ESCUREL_SERVER = "http://escurel-grpc.service.consul:8081"

        GOOGLE_APPLICATION_CREDENTIALS = "/secrets/gcs-sa.json"

        // Per-object key format (substrate.md §4):
        //   <tenant_id>/<YYYY>-<MM>-<DD>T<HH><MM><SS>Z.tar
        // The shipper entrypoint:
        //   1. enumerates active tenants via EscurelAdmin.ListTenants
        //   2. for each: EscurelAdmin.TenantExport → stream tarball
        //   3. validates the SHA-256 terminator (protocol.md contract)
        //   4. uploads to gs://${GCS_BACKUP_BUCKET}/${GCS_BACKUP_PREFIX}<key>
        //   5. emits one audit line: tool=tenant_export_shipped
        // Idempotent: the timestamped key makes a double-fire write two
        // distinct objects; GCS versioning + retention purges old (skill
        // ref 13 idempotency contract).
      }

      // I/O-bound (gRPC stream → GCS), like the substrate's own backup
      // jobs (skill ref 13: 200 MHz / 256 MiB). A large tenant's tarball
      // streams; it is not buffered whole in memory.
      resources {
        cpu    = 300
        memory = 512
      }
    }
  }
}
