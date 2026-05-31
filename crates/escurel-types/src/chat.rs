//! Per-chat-group conversation-history types (M-Chat / issue #63).
//!
//! Proto messages reconciled with the live MCP wire
//! (`chat_message_to_json`, `tool_append_message`,
//! `tool_list_messages`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A stored chat message. MCP wire keys: `chat_group_id`, `msg_id`,
/// `ts`, `role`, `content`, `embedded`, optional `author`, optional
/// `metadata` (object — the proto encoded this as the string
/// `metadata_json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChatMessage {
    pub chat_group_id: String,
    pub msg_id: String,
    pub ts: String,
    pub role: String,
    pub content: String,
    pub embedded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// MCP wire key `metadata` carries a real JSON object (proto used
    /// the string `metadata_json`). Absent when the row has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// `append_message` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppendMessageRequest {
    pub chat_group_id: String,
    pub role: String,
    pub content: String,
    pub author: String,
    pub ts: String,
    /// MCP wire `metadata` object (proto `metadata_json` string).
    pub metadata: Value,
    pub msg_id: String,
    pub embed: bool,
}

/// MCP `append_message` ack: `{msg_id, ts}` (the resolved id +
/// server-stamped timestamp).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppendMessageResponse {
    pub msg_id: String,
    pub ts: String,
}

/// `list_messages` arguments. MCP wire keys: `chat_group_id`,
/// `since`, `until`, `limit`, `cursor`, `direction`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListMessagesRequest {
    pub chat_group_id: String,
    pub since: String,
    pub until: String,
    pub limit: u32,
    pub cursor: String,
    pub direction: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ListMessagesResponse {
    pub messages: Vec<ChatMessage>,
    /// Empty / absent means "no further pages".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
