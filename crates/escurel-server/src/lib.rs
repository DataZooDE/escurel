//! HTTP gateway for Escurel.
//!
//! Today the gateway ships only the substrate-aligned health
//! surface — `/healthz`, `/readyz`, `/version`, `/metrics` —
//! plus the test-driven `serve_on` entry point. The
//! MCP-over-HTTP tool dispatcher and WebSocket endpoint land in
//! follow-up PRs (M3.4b onwards).
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

mod health;
mod server;

pub use health::{ReadinessProbe, ReadinessReport};
pub use server::{ServerConfig, ServerError, ServerHandle, serve};
