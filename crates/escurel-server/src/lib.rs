//! HTTP gateway for Escurel.
//!
//! The gateway ships the substrate-aligned health surface —
//! `/healthz`, `/readyz`, `/version`, `/metrics` — the
//! MCP-over-HTTP dispatcher on `POST /mcp`, the optional gRPC
//! mirror on `:8081`, and the WebSocket scaffolding on `GET /ws`.
//! The WS endpoint authenticates the upgrade with the same
//! [`OidcVerifier`][escurel_auth::OidcVerifier] used by HTTP and
//! gRPC, occupies a session slot on the per-tenant
//! [`QuotaManager`][escurel_quota::QuotaManager], and dispatches
//! the presence + search-subscribe frames defined in
//! `docs/spec/protocol.md §WebSocket framing`. The live CRDT
//! frames (`hello`/`session`, `op`/`op_ack`) are stubbed with a
//! typed error and land in M4.
//!
//! All four endpoints match the substrate-platform skill's
//! runtime contract verbatim:
//!
//! - `/healthz` — dependency-free liveness, always `200 OK`,
//!   body `OK`.
//! - `/readyz` — `200 OK` only when every probed dependency
//!   (LaneStore, Indexer, Embedder) reports up; `503 SERVICE
//!   UNAVAILABLE` otherwise.
//! - `/version` — `200 OK`, body = the `VERSION` env var at
//!   process start.
//! - `/metrics` — Prometheus text exposition. Today returns the
//!   minimal `# HELP` / `# TYPE` shell; the OTel metric exporter
//!   (M5) wires real numbers.

mod grpc;
mod health;
mod mcp;
mod server;
mod session;
mod ws;

pub use health::{AlwaysReady, ReadinessProbe, ReadinessReport};
pub use server::{ServerConfig, ServerError, ServerHandle, serve};
