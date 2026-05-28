// dz-escurel-explore — internal (tailnet-only) Flutter editor for escurel.
//
// Forked from substrate-platform skill `web-service.nomad.hcl` and aligned
// with the auth-portal exposure pattern (ADR-0010 / ref 15-exposure-and-
// ingress, internal-ingress.md):
//
//   1. Single replica (count = 1). The editor has no durable state —
//      restart wipes nothing — so a plain rolling update is sufficient
//      and a second replica would just be load-balanced noise on a
//      tailnet-internal tool.
//
//   2. `meta.exposure = "internal"` + `intprefix-` tag → routed by
//      **fabio-internal** at `https://escurel-explore.<env>.int.data-zoo.de`
//      (tailnet:443, wildcard cert). NEVER on the public LB. Enforced by
//      `check-exposure.sh` at `/deploy-base` time.
//
// Naming uses the `dz-` prefix from day one for the Nomad job ID, but the
// Consul service name is the bare `escurel-explore` so its FQDN under the
// substrate's internal-ingress convention stays clean.
//
// Deploy (operator-side, from the substrate repo):
//   /release-app escurel-explore <env>     # nonprod or prod
// `/release-app` resolves the latest `:main` SHA in GAR, opens a
// bump PR against `ops/base-jobs.manifest`, dispatches /deploy-base
// on merge, and probes /healthz. See references/18-app-image-pipeline.md
// in the substrate-platform skill.

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
  description = "Container image, pinned by SHA tag or digest. NEVER :latest in prod."
  // Example (substrate GAR, pinned by 12-char SHA — matches the tag scheme
  // emitted by .github/workflows/explore.yml and the substrate's
  // build-app-image.yml; see references/18-app-image-pipeline.md):
  //   "europe-west3-docker.pkg.dev/hetzner-agent-backplane/substrate/escurel-explore:abc123def456"
  // For prod, prefer a digest-pinned ref:
  //   "europe-west3-docker.pkg.dev/hetzner-agent-backplane/substrate/escurel-explore@sha256:..."
}

variable "mode" {
  type        = string
  default     = "fixture"
  description = "ESCUREL_EXPLORE_MODE — fixture | http. Stays fixture until escurel-server M3 ships."
}

locals {
  // exposure = internal (ADR-0010): served by fabio-internal at this
  // name via the intprefix- route tag below; the *.<env>.int.data-zoo.de
  // wildcard cert covers it.
  fqdn = "escurel-explore.${var.datacenter}.int.data-zoo.de"
}

job "dz-escurel-explore" {
  type        = "service"
  datacenters = [var.datacenter]
  namespace   = "default"

  meta {
    env = var.datacenter
    // exposure = internal (ADR-0010): routed by fabio-internal via the
    // intprefix- tag below; NEVER on the public LB. Enforced by
    // check-exposure.sh.
    exposure = "internal"
  }

  // No canary/promote: internal app has no public-vs-canary traffic
  // split (never on the public LB), so a plain rolling update is right.
  update {
    auto_revert       = true
    max_parallel      = 1
    min_healthy_time  = "20s"
    healthy_deadline  = "3m"
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
      name     = "escurel-explore" // → escurel-explore.service.consul
      port     = "http"
      provider = "consul"

      // exposure=internal: the `intprefix-` tag makes fabio-internal route
      // https://escurel-explore.<env>.int.data-zoo.de to this service
      // (tailnet-only). NO `urlprefix-` tag, so the public Fabio never
      // routes it. Peers may also bypass fabio-internal and dial
      // `escurel-explore.service.consul:<port>` directly.
      tags = ["intprefix-${local.fqdn}"]

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
        APP                  = "dz-escurel-explore"
        ENV                  = "${var.datacenter}"
        VERSION              = "${var.version}"
        ESCUREL_EXPLORE_MODE = "${var.mode}"
        // ESCUREL_EXPLORE_BASE_URL is baked into the bundle at
        // `flutter build web --dart-define=...`; no env wiring needed
        // here in fixture mode.
      }

      resources {
        cpu    = 100 // MHz — nginx + tiny Flutter bundle is light
        memory = 64  // MiB
      }
    }
  }
}
