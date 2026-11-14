//! Group chats and channels on a chat bridge (#830): which escurel group a
//! shared conversation acts as.
//!
//! A 1:1 chat acts as the person. A group chat or a channel is shared, so an
//! answer there must not carry one member's private access to everyone in it:
//! it acts as a GROUP principal (`group:<g>`) instead, and only once an admin
//! has said which group, with an instance of the admin-only `chat-channel`
//! system skill. The gateway reads those pages when it mints the turn's token
//! (`mint_user_token`), so the grammar lives here for the validator and the
//! gateway to share.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Skill id of the mapping pages: `markdown/instances/chat-channel/<id>.md`.
pub const CHAT_CHANNEL_SKILL: &str = "chat-channel";

/// The subject prefix of a group principal: `group:<g>`.
pub const GROUP_SUBJECT_PREFIX: &str = "group:";

/// The longest escurel group name a mapping (or a Graph lookup) yields.
pub const MAX_GROUP_LEN: usize = 64;

/// The `error.data.code`s `mint_user_token` answers a chat bridge with, in
/// one place so the gateway and the bridge cannot drift apart (crew Q-F6): a
/// renamed code would turn a polite refusal into a retry-until-timeout.
pub mod mint_codes {
    /// A shared chat no `chat-channel` page maps (or one that cannot be mapped).
    pub const UNMAPPED: &str = "chat_channel_unmapped";
    /// Two `chat-channel` pages name different groups for one chat.
    pub const AMBIGUOUS: &str = "chat_channel_ambiguous";
    /// The sender is not a member of the group the chat acts as.
    pub const NOT_MEMBER: &str = "chat_channel_not_member";
    /// The gateway has no group directory, so it answers no shared chat.
    pub const NO_DIRECTORY: &str = "chat_channel_no_directory";
    /// A group directory cannot be reached (retryable).
    pub const DIRECTORY_UNAVAILABLE: &str = "directory_unavailable";
    /// No escurel principal for this sender: the template names a field they
    /// lack, an id is not a plain platform id, or the directory does not know them.
    pub const NO_PRINCIPAL: &str = "no_principal";
    /// The asserted email / UPN is not one the directory knows the sender by.
    pub const IDENTITY_MISMATCH: &str = "identity_mismatch";
    /// The sender's Entra tenant is not the one the gateway looks people up in.
    pub const TENANT_NOT_SERVED: &str = "tenant_not_served";
    /// The gateway does not mint for a chat bridge.
    pub const UNSUPPORTED: &str = "unsupported";
    /// The caller is not a chat bridge's own credential.
    pub const BRIDGE_REQUIRED: &str = "bridge_required";
}

/// What kind of conversation a turn came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationKind {
    /// A 1:1 chat with the bot: acts as the person.
    #[default]
    Personal,
    /// A group chat: acts as its mapped group.
    Group,
    /// A channel thread in a team: acts as the channel's mapped group.
    Channel,
}

impl ConversationKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Group => "group",
            Self::Channel => "channel",
        }
    }
}

/// The conversation a turn came from, in the terms a mapping names it by.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConversationRef {
    pub kind: ConversationKind,
    /// The platform's conversation id (for a channel: the thread's).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
}

impl ConversationRef {
    /// The ids an admin names on a `chat-channel` page for this conversation:
    /// `conversation_id` for a group chat, `team_id` + `channel_id` for a
    /// channel. `None` for a 1:1 chat, and for a channel whose activity named
    /// no team or channel — such a channel cannot be mapped at all, and a
    /// `conversation_id` hint would send the admin to write a page that never
    /// matches (crew Q-F7).
    #[must_use]
    pub fn mapping_hint(&self) -> Option<String> {
        match self.kind {
            ConversationKind::Personal => None,
            ConversationKind::Group => Some(format!("`conversation_id: {}`", self.id)),
            ConversationKind::Channel => match (&self.team_id, &self.channel_id) {
                (Some(team), Some(channel)) => {
                    Some(format!("`team_id: {team}` and `channel_id: {channel}`"))
                }
                _ => None,
            },
        }
    }
}

/// One `chat-channel` page: a group chat (by `conversation_id`) or a channel
/// (by `team_id` + `channel_id`, every thread in it) → an escurel group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatChannelMapping {
    pub channel: String,
    pub target: MappingTarget,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingTarget {
    Conversation(String),
    TeamChannel { team_id: String, channel_id: String },
}

impl ChatChannelMapping {
    /// Read a mapping page's frontmatter (as JSON). Strict: every field is
    /// authority, so a page that does not say exactly one thing maps nothing.
    ///
    /// # Errors
    /// Why the page maps nothing, as one sentence.
    pub fn from_frontmatter(fm: &Value) -> Result<Self, String> {
        let text = |k: &str| {
            fm.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let channel = text("channel").ok_or("`channel:` is required (`msteams`)")?;
        if !crate::bridge::is_valid_channel(&channel) {
            return Err(format!("`channel: {channel}` is not a channel name"));
        }
        let target = match (text("conversation_id"), text("team_id"), text("channel_id")) {
            (Some(id), None, None) => MappingTarget::Conversation(id),
            (None, Some(team_id), Some(channel_id)) => MappingTarget::TeamChannel {
                team_id,
                channel_id,
            },
            _ => {
                return Err(
                    "name either `conversation_id:` (a group chat) or `team_id:` and \
                     `channel_id:` (a channel), not both"
                        .to_owned(),
                );
            }
        };
        let group = text("group").ok_or("`group:` is required (the escurel group it acts as)")?;
        if !is_assignable_group(&group) {
            return Err(format!(
                "`group: {group}` cannot be a group principal (reserved, `escurel:`-prefixed, or \
                 not 1-{MAX_GROUP_LEN} of [A-Za-z0-9._@-])"
            ));
        }
        Ok(Self {
            channel,
            target,
            group,
        })
    }

    /// Whether this mapping covers `conv` on `channel`.
    #[must_use]
    pub fn covers(&self, channel: &str, conv: &ConversationRef) -> bool {
        if self.channel != channel {
            return false;
        }
        match (&self.target, conv.kind) {
            (MappingTarget::Conversation(id), ConversationKind::Group) => *id == conv.id,
            (
                MappingTarget::TeamChannel {
                    team_id,
                    channel_id,
                },
                ConversationKind::Channel,
            ) => {
                conv.team_id.as_deref() == Some(team_id.as_str())
                    && conv.channel_id.as_deref() == Some(channel_id.as_str())
            }
            _ => false,
        }
    }

    /// The subject the conversation acts as.
    #[must_use]
    pub fn subject(&self) -> String {
        format!("{GROUP_SUBJECT_PREFIX}{}", self.group)
    }
}

/// Whether `group` may be granted by a mapping or a directory lookup: a plain
/// name, never a reserved structural group (`public` / `owner` / `admin`) and
/// never an `escurel:` role. The one predicate behind both, so a directory
/// group named `escurel-admin` cannot become the `admin` group by either route.
#[must_use]
pub fn is_assignable_group(group: &str) -> bool {
    !group.is_empty()
        && group.len() <= MAX_GROUP_LEN
        && !group.starts_with("escurel:")
        && !matches!(group, "public" | "owner" | "admin")
        && group
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_mapping_names_one_target_and_a_plain_group() {
        let m = ChatChannelMapping::from_frontmatter(
            &json!({ "channel": "msteams", "conversation_id": "19:g@thread.v2", "group": "sales" }),
        )
        .unwrap();
        assert_eq!(m.subject(), "group:sales");
        let group = ConversationRef {
            kind: ConversationKind::Group,
            id: "19:g@thread.v2".into(),
            ..Default::default()
        };
        assert!(m.covers("msteams", &group));
        assert!(!m.covers("slack", &group));
        let personal = ConversationRef {
            kind: ConversationKind::Personal,
            ..group.clone()
        };
        assert!(
            !m.covers("msteams", &personal),
            "a 1:1 chat is never mapped"
        );

        let c = ChatChannelMapping::from_frontmatter(
            &json!({ "channel": "msteams", "team_id": "t", "channel_id": "c", "group": "ops" }),
        )
        .unwrap();
        let thread = ConversationRef {
            kind: ConversationKind::Channel,
            id: "19:c@thread.tacv2;messageid=1".into(),
            team_id: Some("t".into()),
            channel_id: Some("c".into()),
        };
        assert!(c.covers("msteams", &thread));

        for (fm, why) in [
            (json!({ "conversation_id": "x", "group": "g" }), "channel"),
            (
                json!({ "channel": "msteams", "conversation_id": "x", "team_id": "t",
                         "channel_id": "c", "group": "g" }),
                "not both",
            ),
            (
                json!({ "channel": "msteams", "team_id": "t", "group": "g" }),
                "not both",
            ),
            (
                json!({ "channel": "msteams", "conversation_id": "x" }),
                "group",
            ),
            (
                json!({ "channel": "msteams", "conversation_id": "x", "group": "admin" }),
                "reserved",
            ),
            (
                json!({ "channel": "msteams", "conversation_id": "x", "group": "escurel:admin" }),
                "reserved",
            ),
        ] {
            let err = ChatChannelMapping::from_frontmatter(&fm).unwrap_err();
            assert!(err.contains(why), "{fm}: {err}");
        }
    }

    #[test]
    fn the_mapping_hint_names_only_ids_a_page_can_match() {
        let conv = |kind, team: Option<&str>, channel: Option<&str>| ConversationRef {
            kind,
            id: "19:x@thread".into(),
            team_id: team.map(str::to_owned),
            channel_id: channel.map(str::to_owned),
        };
        assert_eq!(
            conv(ConversationKind::Group, None, None)
                .mapping_hint()
                .as_deref(),
            Some("`conversation_id: 19:x@thread`")
        );
        assert_eq!(
            conv(ConversationKind::Channel, Some("t"), Some("c"))
                .mapping_hint()
                .as_deref(),
            Some("`team_id: t` and `channel_id: c`")
        );
        assert_eq!(
            conv(ConversationKind::Channel, None, Some("c")).mapping_hint(),
            None,
            "a channel without its ids cannot be mapped"
        );
        assert_eq!(
            conv(ConversationKind::Personal, None, None).mapping_hint(),
            None
        );
    }

    #[test]
    fn reserved_and_privileged_names_are_never_assignable() {
        for bad in [
            "",
            "public",
            "owner",
            "admin",
            "escurel:admin",
            "a b",
            "x:y",
        ] {
            assert!(!is_assignable_group(bad), "{bad}");
        }
        assert!(!is_assignable_group(&"g".repeat(MAX_GROUP_LEN + 1)));
        for ok in ["sales", "eng-platform", "team.a_1", "sales@acme.example"] {
            assert!(is_assignable_group(ok), "{ok}");
        }
    }
}
