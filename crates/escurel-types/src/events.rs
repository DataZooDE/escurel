//! Event-sourcing surface (M7): capture / inbox / events / assign.
//!
//! Proto messages reconciled with the live MCP wire (`event_to_json`,
//! `tool_capture_event`, `tool_assign_event`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::null::null_as_default;

/// A captured event. MCP wire keys: `event_id`, `at`, `source`,
/// `mime`, `label_skill`, `instance_page_id`, `status`, `title`,
/// `body`, `provenance` (a JSON value — the proto encoded this as the
/// string `provenance_json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Event {
    pub event_id: String,
    /// `null` on the wire when the event carries no timestamp.
    #[serde(deserialize_with = "null_as_default")]
    pub at: String,
    pub source: String,
    pub mime: String,
    pub label_skill: String,
    /// `null` on the wire for an unassigned inbox event.
    #[serde(deserialize_with = "null_as_default")]
    pub instance_page_id: String,
    /// `inbox` | `processed`.
    pub status: String,
    pub title: String,
    pub body: String,
    /// MCP wire key `provenance` carries a real JSON value (`null`
    /// when absent); the proto encoded it as the string
    /// `provenance_json`.
    pub provenance: Value,
}

/// `capture_event` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CaptureEventRequest {
    pub event_id: String,
    pub at: String,
    pub source: String,
    pub mime: String,
    pub label_skill: String,
    pub instance_page_id: String,
    pub title: String,
    pub body: String,
    /// MCP wire `provenance` object (proto `provenance_json` string).
    pub provenance: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListInboxRequest {
    pub limit: u32,
    /// Resume cursor from a previous page's `next_cursor`. Empty = first page.
    pub cursor: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListInboxResponse {
    pub events: Vec<Event>,
    /// Present iff rows lie past the page — pass back as `cursor` to
    /// continue. **Only its absence means the listing is complete**; a
    /// short page never does (the per-event ACL filter runs after the
    /// limit and legitimately shortens pages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListEventsRequest {
    pub instance_page_id: String,
    pub limit: u32,
    /// Look a **single event up by id** instead of listing an instance's
    /// history. When set, `instance_page_id` is ignored and the response
    /// carries just that event — including the `instance_page_id` that
    /// `assign_event` bound it to.
    ///
    /// This exists because there was otherwise no way to ask *where an event
    /// went*. Reconciling a run whose trigger had no pre-flagged target — the
    /// ordinary shape for an LLM harness, which decides for itself which
    /// instance to fold an event into — could see the event leave the inbox
    /// but never learn which page it landed on, so the effect could not be
    /// confirmed and the run never cascaded.
    pub event_id: Option<String>,
    /// Resume cursor from a previous page's `next_cursor` (listing
    /// branch only; meaningless with `event_id`). Empty = first page.
    pub cursor: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListEventsResponse {
    pub events: Vec<Event>,
    /// See [`ListInboxResponse::next_cursor`] — same contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `assign_event` arguments. MCP wire keys: `event_id`,
/// `instance_page_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AssignEventRequest {
    pub event_id: String,
    pub instance_page_id: String,
}

/// MCP `assign_event` ack: `{event_id, instance_page_id, status}`.
/// (The proto `AssignEventResponse` carries only `event_id` +
/// `instance_page_id`; the live wire adds `status: "processed"`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AssignEventResponse {
    pub event_id: String,
    pub instance_page_id: String,
    pub status: String,
}
