//! Core value objects shared across the wire contract.

use serde::{Deserialize, Serialize};

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
/// stands in for "absent" (proto3 has no nullable string).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WikilinkParsed {
    pub skill: String,
    pub id: String,
    pub anchor: String,
    pub version: String,
    pub alias: String,
}
