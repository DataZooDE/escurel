//! HTTP gateway for Escurel.
//!
//! The gateway ships the substrate-aligned health surface —
//! `/healthz`, `/readyz`, `/version`, `/metrics` — the
//! MCP-over-HTTP dispatcher on `POST /mcp`, and the WebSocket
//! scaffolding on `GET /ws`.
//! The WS endpoint authenticates the upgrade with the same
//! [`OidcVerifier`][escurel_auth::OidcVerifier] used by HTTP,
//! occupies a session slot on the per-tenant
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
//! - `/metrics` — Prometheus text exposition. Wired through
//!   `escurel-obs::Metrics` (PR M5.1b): the dispatcher debits
//!   `escurel_requests_total{route, status}` on every accepted
//!   request, the latency histogram is observed in the same
//!   place, and `escurel_up` flips to `1` at `serve()` start.
//!   The OTLP trace exporter is opt-in via
//!   `ESCUREL_OBSERVABILITY_OTLP_ENDPOINT` (or the deprecated
//!   `ESCUREL_OTLP_ENDPOINT`); when unset traces are a no-op.

// The single `tools_list_payload` `json!` literal enumerates the whole MCP
// tool surface; each added tool deepens the macro's recursive expansion.
// The provenance-graph tools (ADR-0010) pushed it past the default 128.
#![recursion_limit = "256"]

mod auth_gate;
pub mod config;
mod config_probe;
mod health;
mod live_dispatch;
mod mcp;
pub mod pack;
mod remote_backend;
pub mod selfpack;
mod server;
mod session;
pub mod snapshot_publish;
pub mod snapshot_refresh;
mod tenant_archive;
mod webhook;
mod ws;

pub use config::{BootedServer, ConfigError, EscurelConfig};
pub use health::{AlwaysReady, ReadinessProbe, ReadinessReport};
pub use server::{
    AutonomyLintMode, DraftMode, EmbedderFactory, EventAclMode, ServerConfig, ServerError,
    ServerHandle, WriteAclMode, serve,
};
