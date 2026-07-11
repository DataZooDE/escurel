//! Plain `serde`-derived Rust structs for the escurel MCP-over-HTTP
//! wire contract.
//!
//! These types are the intended single source of truth for the
//! request/response/message shapes exchanged over the MCP `/mcp`
//! endpoint.
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
mod chat;
mod core;
mod events;
mod null;
mod session;
mod workflow;

pub use admin::*;
pub use agent::*;
pub use chat::*;
pub use core::*;
pub use events::*;
pub use session::*;
pub use workflow::*;
