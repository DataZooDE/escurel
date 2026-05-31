//! Event-sourcing surface (M7): capture / inbox / events / assign.
//!
//! Proto messages reconciled with the live MCP wire (`event_to_json`,
//! `tool_capture_event`, `tool_assign_event`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A captured event. MCP wire keys: `event_id`, `at`, `source`,
/// `mime`, `label_skill`, `instance_page_id`, `status`, `title`,
/// `body`, `provenance` (a JSON value — the proto encoded this as the
/// string `provenance_json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Event {
    pub event_id: String,
    pub at: String,
    pub source: String,
    pub mime: String,
    pub label_skill: String,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListInboxResponse {
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListEventsRequest {
    pub instance_page_id: String,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListEventsResponse {
    pub events: Vec<Event>,
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
