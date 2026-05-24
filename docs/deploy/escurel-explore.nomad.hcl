// dz-escurel-explore — tailnet-only Flutter editor for escurel.
//
// Forked from substrate-platform skill `web-service.nomad.hcl`.
// Diverges in two ways from the public-web template:
//
//   1. Single replica (count = 1, canary = 1). The editor has no
//      durable state — restart wipes nothing — so blue/green is
//      sufficient and the second replica would just be load-balanced
//      noise on a tailnet-internal tool.
//
//   2. **No `urlprefix-*` Fabio tag.** The service is reachable only
//      via Consul DNS at `escurel-explore.service.consul`. From a
//      tailnet-joined laptop with MagicDNS this is the path; Fabio
//      never sees this service. Per substrate skill reference 03.
//
// Naming uses the post-M5 `dz-` prefix from day one so this job
// does not need a rename when the rest of escurel reconciles to
// the substrate naming convention.
//
// Deploy (operator-side, from the substrate repo):
//   /deploy-green dz-escurel-explore <version>
//   /promote dz-escurel-explore

variable "datacenter" {
  type        = string
  description = "Nomad datacenter (env): nonprod | prod."
}

variable "version" {
  type        = string
  description = "Build version. Convention: <YYYY-MM-DD>-<git-short-sha>."
}

variable "image" {
  type        = string
  description = "Container image, pinned by digest. NEVER :latest in prod."
  // Example: "ghcr.io/datazoode/escurel-explore@sha256:abc..."
}

variable "mode" {
  type        = string
  default     = "fixture"
  description = "ESCUREL_EXPLORE_MODE — fixture | http. Stays fixture until escurel-server M3 ships."
}

job "dz-escurel-explore" {
  type        = "service"
  datacenters = [var.datacenter]

  update {
    canary            = 1
    auto_promote      = false
    auto_revert       = true
    max_parallel      = 1
    min_healthy_time  = "10s"
    healthy_deadline  = "2m"
    progress_deadline = "5m"
  }

  group "web" {
    count = 1

    network {
      mode = "bridge"
      port "http" {
        to = 8080
      }
    }

    service {
      name     = "escurel-explore"     // → escurel-explore.service.consul
      port     = "http"
      provider = "consul"

      // Tailnet-only: no Fabio routing. Peers find this service via
      // Consul DNS only. To make it publicly reachable, add a
      // `urlprefix-<fqdn>` tag here (and pick an fqdn).
      tags        = []
      canary_tags = []

      check {
        type     = "http"
        path     = "/healthz"
        interval = "10s"
        timeout  = "2s"
      }
    }

    task "app" {
      driver = "docker"

      config {
        image = var.image
        ports = ["http"]
      }

      env {
        APP                    = "dz-escurel-explore"
        ENV                    = "${var.datacenter}"
        VERSION                = "${var.version}"
        ESCUREL_EXPLORE_MODE   = "${var.mode}"
        // ESCUREL_EXPLORE_BASE_URL is baked into the bundle at
        // `flutter build web --dart-define=...`; no env wiring needed
        // here in fixture mode.
      }

      resources {
        cpu    = 100  // MHz — nginx + tiny Flutter bundle is light
        memory = 64   // MiB
      }
    }
  }
}
