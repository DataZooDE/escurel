//! The seam between a messaging platform and escurel's page chat. Teams is
//! the one implementation today ([`crate::teams::TeamsChannel`]); Slack fits
//! the same four verbs: verify a webhook, read a turn out of it, post a reply,
//! show that work is in progress.

use std::future::Future;

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One person's message, in channel-neutral terms. Persisted as JSON by the
/// bridge's state store, so a restart can finish a turn it had started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundTurn {
    /// The platform's id for this message (unique within its conversation).
    pub message_id: String,
    pub conversation_id: String,
    /// The platform user as the principal template sees it
    /// (`{aadObjectId}`, `{userId}`, `{tenantId}` for Teams).
    pub user: UserRef,
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// What the channel needs to answer this turn later (for Teams: the
    /// activity's `serviceUrl`, `conversation`, `from`, `recipient`).
    pub reply_to: Value,
}

impl InboundTurn {
    /// The turn's identity across the bridge: a platform's message id is
    /// unique only within its conversation (Teams activity ids are), so the
    /// dedup key, the user turn's `msg_id` and the event id all derive from
    /// both.
    #[must_use]
    pub fn key(&self) -> String {
        short_hash(&[&self.conversation_id, &self.message_id])
    }
}

/// 32 hex chars of a length-prefixed SHA-256 over `parts` (so `("ab", "c")`
/// and `("a", "bc")` differ).
pub(crate) fn short_hash(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.len().to_le_bytes());
        h.update(p.as_bytes());
    }
    format!("{:x}", h.finalize())[..32].to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserRef {
    pub id: String,
    pub directory_id: Option<String>,
    pub directory_tenant: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub url: String,
}

/// Why an inbound request was refused. Every variant is a `401`: the caller
/// could not prove it is the platform.
#[derive(Debug, thiserror::Error)]
pub enum InboundError {
    #[error("no bearer token")]
    MissingToken,
    #[error("token rejected: {0}")]
    BadToken(String),
    #[error("signing keys unavailable: {0}")]
    Keys(String),
    #[error("malformed activity: {0}")]
    Malformed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("channel credentials: {0}")]
    Auth(String),
    #[error("channel http: {0}")]
    Http(String),
    #[error("channel refused ({status}): {body}")]
    Refused { status: u16, body: String },
}

/// A messaging platform the bridge answers on.
pub trait ChatChannel: Send + Sync + 'static {
    /// The channel's name: the `source` of the captured event and the
    /// namespace of message ids in the state store.
    fn name(&self) -> &'static str;

    /// Authenticate a webhook request and return its payload.
    fn verify_inbound(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> impl Future<Output = Result<Value, InboundError>> + Send;

    /// The turn to answer in a verified payload; `None` for anything that is
    /// not a person's message (a join, a reaction, the bot's own echo).
    fn to_capture(&self, payload: &Value) -> Option<InboundTurn>;

    /// Post `text` as the one answer to `turn`.
    fn send_reply(
        &self,
        turn: &InboundTurn,
        text: &str,
    ) -> impl Future<Output = Result<(), ChannelError>> + Send;

    /// Show that an answer is being worked on.
    fn typing(&self, turn: &InboundTurn) -> impl Future<Output = Result<(), ChannelError>> + Send;

    /// Download an attachment's bytes, at most `max_bytes`.
    fn fetch_attachment(
        &self,
        turn: &InboundTurn,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> impl Future<Output = Result<Vec<u8>, ChannelError>> + Send;
}
