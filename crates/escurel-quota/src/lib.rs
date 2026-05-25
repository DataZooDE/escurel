//! Per-tenant token-bucket rate limiter + concurrent-session cap.
//!
//! Three rate dimensions per
//! `docs/spec/platform.md §Quotas`:
//!
//! - `queries_per_minute` — all read tools
//! - `writes_per_minute` — `update_page`, `apply_op`,
//!   `close_session(commit=true)`. Also debits `embeds_per_minute`
//!   when a write triggers embedding.
//! - `embeds_per_minute` — counts embedding jobs.
//! - `concurrent_sessions` — semaphore over open MCP / WS / gRPC
//!   sessions.
//!
//! All in-memory: restart zeroes every bucket. Per the spec this
//! is intentional — buckets are a rate-shaping device, not a
//! billing system. Durable accounting lives in the OTel pipeline.

mod manager;
mod token_bucket;

pub use manager::{Dimension, QuotaConfig, QuotaError, QuotaManager, QuotaSnapshot, SessionGuard};
pub use token_bucket::{QuotaExhausted, TokenBucket};
