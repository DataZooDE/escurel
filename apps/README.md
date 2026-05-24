# apps/

Top-level home for Flutter / web companion apps that live alongside
the escurel server. Each subdirectory is one independent application
with its own `pubspec.yaml`, `Dockerfile`, and CI workflow.

**Rule:** apps may depend on escurel's HTTP / gRPC API; they do **not**
depend on the Rust crates directly. If an app needs server behaviour
that doesn't exist yet, the right place to put it is the server, not
inlined into the app's Dart code.

## Inhabitants

| dir | what it is |
|---|---|
| [`escurel-explore/`](escurel-explore/) | General Flutter editor on top of escurel — tracks every backend capability as it lands. Deployed tailnet-only as `dz-escurel-explore` on the substrate. |
