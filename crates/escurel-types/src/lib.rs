//! Plain `serde`-derived Rust structs for the escurel MCP-over-HTTP
//! wire contract, plus the small shared vocabularies more than one crate
//! must agree on.
//!
//! The flat types are the intended single source of truth for the
//! request/response/message shapes exchanged over the MCP `/mcp`
//! endpoint. The namespaced modules are the contracts BETWEEN escurel's own
//! processes that no single crate may own: the agent-profile parser
//! (`agent_profile`, read by the runner, the gateway and `validate`), the
//! `run-finished` record (`run_record`, written by the runner, folded by the
//! gateway), the tool and harness vocabularies (`tools`, `harness`), and,
//! behind features, the cron grammar (`schedule`) and the credential
//! scrubber (`redact`). Each stays dependency-light: no I/O, no async.
//!
//! Wire-shape rules captured here:
//!
//! * Every struct derives `Serialize` + `Deserialize` and uses
//!   `#[serde(default)]` so that missing JSON keys deserialize to the
//!   type default. The MCP wire omits empty/zero fields, so a decoder
//!   must tolerate their absence.
//! * Where the MCP wire key differs from the idiomatic Rust field
//!   name we use `#[serde(rename = "...")]`. The known divergences are
//!   documented on the affected fields.
//! * Where the proto stores a value as a JSON-encoded *string* but the
//!   MCP wire carries a real JSON *value* (frontmatter, rows, params,
//!   payload), the Rust field is `serde_json::Value`. The MCP
//!   transport is the contract now, not the proto string-encoding.
//! * Genuinely optional sub-objects are `Option<T>` (e.g.
//!   `ResolveResponse.page`).

mod admin;
mod agent;
// Namespaced like `pack`: `escurel_types::agent_profile::glob_match` reads better than a flat helper.
pub mod agent_profile;
pub mod call_result;
mod chat;
mod core;
mod drafts;
mod events;
pub mod evolve_labels;
pub mod harness;
mod null;
// Public as a MODULE (not glob-re-exported functions): the pack
// signing/verification primitives read best namespaced —
// `escurel_types::pack::verify_pack` — while the manifest TYPES stay
// flat like every other wire type.
pub mod pack;
/// Credential shapes shared by the pack gate and every scrubber (feature `redact`).
#[cfg(feature = "redact")]
pub mod redact;
// Namespaced: `escurel_types::run_record::RunFinishedBody` says whose record it is.
pub mod run_record;
/// `schedule:` cron grammar shared by the validator and the runner (feature `schedule`).
#[cfg(feature = "schedule")]
pub mod schedule;
mod session;
// Namespaced: `escurel_types::tools::is_known_tool` reads better than a flat helper.
pub mod tools;
mod workflow;

pub use admin::*;
pub use agent::*;
pub use chat::*;
pub use core::*;
pub use drafts::*;
pub use events::*;
pub use pack::*;
pub use session::*;
pub use workflow::*;
