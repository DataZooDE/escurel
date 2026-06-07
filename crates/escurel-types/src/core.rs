//! Core value objects shared across the wire contract.

use serde::{Deserialize, Serialize};

use crate::null::null_as_default;

/// A reference to a page. Mirrors the proto `PageRef` /
/// escurel-index `PageRef`. `page_type` is `"skill"` | `"instance"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PageRef {
    pub page_id: String,
    pub slug: String,
    pub skill: String,
    pub page_type: String,
}

/// The parsed components of a wikilink. Each empty-string segment
/// stands in for "absent" (proto3 has no nullable string). The MCP
/// wire may emit an explicit `null` for an absent `anchor` / `version`
/// / `alias`, so those tolerate `null` as the empty default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WikilinkParsed {
    #[serde(deserialize_with = "null_as_default")]
    pub skill: String,
    // A bare-skill wikilink (`[[customer]]`) resolves with no `id`
    // segment; the MCP wire emits an explicit `null` there, so tolerate
    // it as the empty default rather than failing the decode.
    #[serde(deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(deserialize_with = "null_as_default")]
    pub anchor: String,
    #[serde(deserialize_with = "null_as_default")]
    pub version: String,
    #[serde(deserialize_with = "null_as_default")]
    pub alias: String,
}
