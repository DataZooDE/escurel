# ADR-0011 — Self-packaging: a single-binary deploy that carries its corpus

**Status:** Accepted, 2026-07-31.
**Builds on:** [ADR-0001](0001-duckdb-only-storage.md) (canonical markdown
corpus + a *derived* DuckDB index — everything is rebuildable from the
corpus), [ADR-0005](0005-page-layer-model.md) (base vs overlay layers),
and [ADR-0006](0006-skill-packs.md) (the secret-free content scrubber,
reused here).
**Prior art:** DataZoo **flAPI**'s `flapi pack` — it folds a config tree
into the serving binary (a ZIP appended after the executable on
Linux/Windows, or a reserved Mach-O segment on macOS), reverse-scanned at
startup, with filesystem fallback and a pack-time secret deny list. This
ADR adapts that pattern to escurel.

## Context

Today the `escurel-server` download is the **engine** (the binaries +
bundled libduckdb). A vertical's ontology — e.g. the `project-memory`
skill pack — is markdown **content** in the repo (`examples/…`), imported
or seeded into a tenant (`ESCUREL_SEED_DIR`, `import_pack`). That split is
deliberate (skills are editable, versioned, per-tenant content, not code —
see ADR-0006), but it has a real UX cost: *"download the binary"* does not
give *"a running, populated tenant"* — you also have to fetch a corpus.

flAPI solves the analogous problem for its config tree with `flapi pack`:
one `scp`-able self-contained executable. We want the same one-file deploy
for escurel: `escurel-server-projmem` that boots into a populated
project-memory tenant with nothing else to ship.

## Decision

**`escurel-server` self-packages a markdown corpus into a copy of
itself**, via three subcommands:

- `escurel-server pack --in <dir> --out <bin>` — append a bundle of
  `<dir>` to a copy of the running server binary.
- `escurel-server info` — report whether the running binary carries a
  bundle, and list its entries.
- `escurel-server unpack --to <dir>` — extract the carried bundle.

**Bundle format (Linux/Windows).** The bundle is a **deterministic
`tar.gz`** of the corpus (reusing `escurel_server::pack::build_tarball`,
which already pins `mtime=uid=gid=0, mode=0644` → byte-reproducible with
no `SOURCE_DATE_EPOCH` needed), appended **after the executable's EOF**,
followed by a fixed 16-byte trailer: an 8-byte magic (`ESCPACK1`) + the
`u64`-LE bundle length. At startup the binary reads its own image
(`current_exe`), checks the tail for the magic, and — if present — recovers
the bundle by seeking back `len + 16` bytes. No trailer ⇒ no bundle ⇒ the
binary behaves exactly as today (the fallback is the current behaviour, so
existing operators see no change). macOS uses the Linux/Windows
append-after-EOF layout initially (`--macos-append`), with the
notarisable reserved-segment variant deferred (see *Deferred*).

**Boot semantics: seed-on-first-boot, not a live embedded read layer.**
When the binary carries a bundle, its corpus is **extracted into the
tenant's `LaneStore`** at boot through the existing seed path (the same
mechanism as `ESCUREL_SEED_DIR`), *if the tenant corpus is fresh/empty*.
It is **not** mounted as a live in-memory read layer.

*Why this differs from flAPI.* flAPI's config is read-only at runtime, so a
live `embed://` read layer with fallback is natural. escurel's corpus is
**read-write and derivable**: an author edits pages, and the DuckDB index +
`LaneStore::url` (DuckDB needs real `file://` URLs) both assume the corpus
lives in a real store. Extracting the bundle into the writable `LaneStore`
keeps every page a real, editable, URL-addressable file, reuses
audit-and-rebuild verbatim (ADR-0001), and needs no new `LaneStore` impl.
An existing (non-empty) tenant is left untouched — the operator's data
wins; `ESCUREL_SEED_DIR`, if set, takes precedence over the embedded
bundle (explicit over implicit).

**Secret-free by construction.** `pack` runs every file through the
existing `pack_scrub_rejection` deny set (`*.env`-shaped, DSNs with inline
credentials, PEM/PGP private keys, `password=` connection strings) and
**refuses the pack** on a hit (`--allow-secrets` for tests only).
Credentials come from the environment at runtime, never the bundle —
consistent with the pack model (INV-SECRETFREE).

## Considered alternatives

- **Attach a signed pack tarball as a release asset** (no binary change).
  Simplest, but two files to deploy and a manual `import_pack` step — it
  doesn't deliver the one-`scp` story. Kept as a complementary option, not
  the headline.
- **Embed only a fixed default pack via `include_dir!` + `escurel seed
  project-memory`.** Smaller, but bakes *one* vertical into the binary and
  can't carry an arbitrary operator corpus. Rejected as the primary design
  (it's a strict subset of `pack`).
- **Live in-memory `EmbeddedLaneStore` base + `FsStore` overlay
  (flAPI-faithful).** Purest mapping to the layer model, but `LaneStore`
  requires a real `url()` (DuckDB `file://`), which an in-memory store
  can't satisfy, and it fights the read-write/derivable model. Rejected in
  favour of seed-on-first-boot.

## Consequences

- **What this enables:** `escurel-server pack --in examples/project-memory
  --out escurel-projmem` → `scp escurel-projmem host && ./escurel-projmem`
  boots a populated project-memory tenant, no repo clone, no seed dir. The
  seeded pages are ordinary editable content thereafter.
- **What does NOT change:** an unbundled `escurel-server` is byte-behaviour
  identical to today; the storage/index/layer model is untouched; the
  bundle is just an alternate seed source.
- **Reproducible** by construction (`build_tarball` pins tar metadata), so
  `pack` output is byte-identical across runs — bundle diffs are
  meaningful.

## Deferred

- **macOS notarisable reserved-segment** (flAPI's Mach-O `__bundle`
  segment + re-codesign). The append-after-EOF layout works on macOS but
  isn't notarisable; the segment variant is a follow-on.
- **Live base/overlay layering of the bundle** (bundle as a read-only
  `base@` layer rather than a first-boot seed) — only if a use case needs
  the bundle to stay authoritative across reboots against a non-empty
  tenant.

## Increments

1. **Bundle codec** (this ADR's first PR): `escurel_server::selfpack` —
   deterministic build + append + trailer + read-back + unpack + list,
   with the secret scrub wired in. Unit-tested (round-trip, trailer
   detection, no-bundle fallback, secret refusal, determinism). No boot or
   CLI wiring yet — de-risks the core.
2. **CLI + boot** — `pack`/`unpack`/`info` subcommands on `escurel-server`
   and the first-boot seed hook in `config`; a no-mock test that packs a
   corpus, runs the bundled server, and serves the seeded skills.
3. **`release.yml` bundled asset (done)** — each release build folds
   `examples/project-memory` into the staged server and publishes an
   `escurel-<ver>-<target>-project-memory` archive (Linux + Windows),
   smoke-tested in CI (boots + `list_skills` shows the seeded ontology).
   So a *download* — not just a local `pack` — gives the full experience.
3b. **macOS notarisable reserved-segment (deferred)** — the append-after-EOF
   layout invalidates a macOS code signature, so the macOS bundled asset is
   omitted for now; the notarisable Mach-O reserved-segment variant (a
   macOS linker + re-`codesign` path) is the remaining follow-on.
