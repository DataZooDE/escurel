// escurel — golden-image bake FRAGMENT.
//
// ⚠️  THIS IS A FRAGMENT, NOT A STANDALONE BUILD ⚠️
//
// The DataZoo Hetzner substrate bakes ONE golden image for all
// `tag:cli` nodes via `packer/golden.pkr.hcl` in the substrate repo
// (DataZooDE/hetzner-agent-substrate). This file is the escurel-
// specific *delta* that the substrate operator folds into that
// pipeline per docs/deploy/substrate.md §6. It is NOT run on its own:
// there is no `source`/`build` driver block, no base AMI/snapshot
// selection, no Hetzner builder credentials here. Those all live in
// the substrate's golden.pkr.hcl. Copy the provisioner blocks below
// into that file (or add this as an included `.pkr.hcl` in the same
// `packer/` directory and reference its provisioners).
//
// Why bake (not pull-on-start): the EmbeddingGemma artefact is
// ~600 MiB and the candle path loads it from a LOCAL directory
// (CandleEmbedder::from_local, see crates/escurel-embed/src/candle.rs).
// Pulling at boot would need egress allowance and breaks the
// substrate's SPEC §6 default-deny posture. Bake-into-image keeps
// the node air-gappable.
//
// What this fragment adds to the golden image (substrate.md §6):
//   1. candle runtime libs (pure-Rust; no libtorch / onnxruntime).
//   2. EmbeddingGemma-300m model artefact at
//      /opt/escurel/models/embeddinggemma-300m/   (~600 MiB)
//      referenced by ESCUREL_EMBEDDING_MODEL in escurel.nomad.hcl.
//   3. DuckDB pinned with vss + fts extensions pre-loaded.
//   4. escurel-server static Rust binary at
//      /usr/local/bin/escurel-server.
//
// The model artefact is NOT committed to git or fetched from the
// public HF Hub at bake time inside the substrate's network-restricted
// build. The operator stages it from the substrate's own artefact
// store (a private object-storage prefix / internal mirror). The
// `model_source_url` variable below is a PLACEHOLDER for that internal
// source — an operator-only value; do not point it at huggingface.co.

variable "model_source_url" {
  type        = string
  description = "Operator-only: internal mirror/object-store URL for the EmbeddingGemma-300m artefact tarball (config.json + tokenizer.json + model.safetensors). NOT the public HF Hub. Placeholder — set in the substrate pipeline."
  default     = "<substrate-internal-artefact-store>/escurel/embeddinggemma-300m.tar.zst"
}

variable "escurel_server_binary_url" {
  type        = string
  description = "Operator-only: internal artefact-store URL for the escurel-server static binary built by CI. Placeholder — set in the substrate pipeline."
  default     = "<substrate-internal-artefact-store>/escurel/escurel-server"
}

// ---------------------------------------------------------------
// Fold these provisioner blocks into packer/golden.pkr.hcl's
// `build { ... }`. They assume the substrate's standard
// shell-provisioner conventions (run as root during bake).
// ---------------------------------------------------------------

// 1 + 2 — EmbeddingGemma model artefact, baked to the path
//         ESCUREL_EMBEDDING_MODEL points at.
provisioner_shell_embeddinggemma = <<EOT
  set -euo pipefail
  install -d -m 0755 /opt/escurel/models/embeddinggemma-300m
  # Stage from the substrate's internal artefact store (NOT public HF).
  curl -fsSL "${var.model_source_url}" -o /tmp/eg300m.tar.zst
  tar --zstd -xf /tmp/eg300m.tar.zst -C /opt/escurel/models/embeddinggemma-300m
  rm -f /tmp/eg300m.tar.zst
  # Sanity: the candle loader needs exactly these three files.
  test -f /opt/escurel/models/embeddinggemma-300m/config.json
  test -f /opt/escurel/models/embeddinggemma-300m/tokenizer.json
  test -f /opt/escurel/models/embeddinggemma-300m/model.safetensors
EOT

// candle is a pure-Rust dependency statically linked into the
// escurel-server binary, so there are NO system runtime libs to
// install (this is the whole point of choosing candle over
// libtorch/onnxruntime — substrate.md §6). This block is intentionally
// a no-op note; if a future build switches to a CUDA/Metal feature
// it would add the device runtime here.
provisioner_shell_candle_runtime = <<EOT
  set -euo pipefail
  echo "candle is pure-Rust, statically linked into escurel-server; no runtime libs to bake on the CPU path."
EOT

// 4 — escurel-server static binary.
provisioner_shell_escurel_server = <<EOT
  set -euo pipefail
  curl -fsSL "${var.escurel_server_binary_url}" -o /usr/local/bin/escurel-server
  chmod 0755 /usr/local/bin/escurel-server
  /usr/local/bin/escurel-server --version
EOT

// 3 — DuckDB with vss + fts pre-loaded. escurel links libduckdb-sys,
//     so the extensions must be present on the image so first-request
//     index build does not need egress to the DuckDB extension repo.
//     The exact pin is whatever the workspace Cargo.lock resolves
//     libduckdb-sys to; the operator reads it from the build and pins
//     the matching extension binaries here.
provisioner_shell_duckdb_extensions = <<EOT
  set -euo pipefail
  install -d -m 0755 /opt/escurel/duckdb-extensions
  # Operator stages vss + fts .duckdb_extension binaries matching the
  # libduckdb-sys version from the escurel workspace Cargo.lock into
  # this dir; the binary's DuckDB connection LOADs them from here with
  # no network fetch. Source URL is the substrate-internal mirror
  # (placeholder), not the public extension repo.
  echo "stage vss + fts extension binaries for the pinned DuckDB version into /opt/escurel/duckdb-extensions"
EOT
