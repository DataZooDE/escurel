//! Microsoft Teams over the Bot Framework.
//!
//! Inbound: the channel POSTs an activity with a JWT the Bot Framework signed
//! (RS256). It is checked against the keys the OpenID metadata names, for the
//! configured issuer, with `aud` = this bot's app id, and its `serviceurl`
//! claim must be the activity's `serviceUrl` — so a token for one connector
//! cannot carry an activity that points the reply somewhere else. That bound
//! `serviceUrl` is the only host the bridge ever sends its connector bearer to.
//! The activity must be a Teams one (`channelId: msteams`), and when the JWKS
//! lists the signing key's `endorsements` they must include `msteams`, so a
//! token the Bot Framework issued for another channel this bot is enabled on
//! cannot enter the Teams identity path.
//! The check is `escurel_auth::bot_framework::ActivityVerifier`, the one the
//! gateway also runs on the forwarded token (#835); the bearer is handed on
//! with the payload for the turn's mint, never stored.
//!
//! Only a person in an allowed Entra tenant is answered: in a `personal` chat,
//! or — when the message @mentions the bot — in a `groupChat` or a `channel`
//! thread (each then acts as the group an admin mapped it to, #830; the
//! mention is stripped). Everything else is ignored.
//!
//! Outbound: a client-credentials token from the configured token endpoint,
//! then `POST {serviceUrl}/v3/conversations/{id}/activities[/{replyToId}]`: a
//! 1:1 chat answers the activity, a group chat posts into the conversation, a
//! channel answers the thread's root. The sender's email / UPN (for an
//! email-keyed group directory) comes from
//! `GET {serviceUrl}/v3/conversations/{id}/members/{userId}` with the same
//! bearer — no Microsoft permission beyond the bot's own.

use std::time::Duration;

use axum::http::HeaderMap;
use escurel_auth::ClientCredentials;
use escurel_auth::bot_framework::{
    ActivityTokenError, ActivityVerifier, ConversationMember, TEAMS_CHANNEL as CHANNEL_ID,
    member_url, path_segment,
};
use escurel_client::SecretString;
use serde_json::{Value, json};

use escurel_types::chat_channel::{ConversationKind, ConversationRef};

use crate::channel::{
    Attachment, ChannelError, ChatChannel, InboundError, InboundTurn, MemberProfile, UserRef,
    Verified,
};
use crate::config::TeamsConfig;

/// The Teams file-consent attachment that carries a pre-authorised download URL.
const TEAMS_FILE: &str = "application/vnd.microsoft.teams.file.download.info";

pub struct TeamsChannel {
    cfg: TeamsConfig,
    http: reqwest::Client,
    verifier: ActivityVerifier,
    connector: ClientCredentials,
}

impl TeamsChannel {
    #[must_use]
    pub fn new(cfg: TeamsConfig) -> Self {
        let connector = ClientCredentials::new(
            cfg.token_url.clone(),
            cfg.app_id.clone(),
            cfg.app_password.clone(),
            cfg.token_scope.clone(),
        );
        let verifier = ActivityVerifier::new(
            cfg.app_id.clone(),
            cfg.issuer.clone(),
            cfg.openid_metadata_url.clone(),
        );
        Self {
            cfg,
            http: client(reqwest::Client::builder()),
            verifier,
            connector,
        }
    }

    async fn connector_token(&self) -> Result<String, ChannelError> {
        self.connector
            .token(&self.http)
            .await
            .map_err(|e| ChannelError::Auth(e.to_string()))
    }

    async fn post_activity(&self, url: String, body: Value) -> Result<(), ChannelError> {
        let token = self.connector_token().await?;
        let resp = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    ChannelError::Connect(e.to_string())
                } else {
                    ChannelError::Http(e.to_string())
                }
            })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        Err(ChannelError::Refused {
            status: status.as_u16(),
            body: resp.text().await.unwrap_or_default(),
        })
    }
}

impl ChatChannel for TeamsChannel {
    fn name(&self) -> &'static str {
        "msteams"
    }

    async fn verify_inbound(
        &self,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<Verified, InboundError> {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(InboundError::MissingToken)?;
        let activity: Value =
            serde_json::from_slice(body).map_err(|e| InboundError::Malformed(e.to_string()))?;
        let service_url = activity["serviceUrl"].as_str().unwrap_or_default();
        self.verifier
            .verify(token, service_url)
            .await
            .map_err(|e| match e {
                ActivityTokenError::BadToken(why) => InboundError::BadToken(why),
                ActivityTokenError::Keys(why) => InboundError::Keys(why),
            })?;
        if activity["channelId"].as_str() != Some(CHANNEL_ID) {
            return Err(InboundError::BadToken(format!(
                "not a {CHANNEL_ID} activity (channelId {})",
                activity["channelId"]
            )));
        }
        // The gateway re-checks this token for the turn's mint (#835).
        Ok(Verified {
            payload: activity,
            proof: Some(SecretString::from(token.to_owned())),
        })
    }

    fn to_capture(&self, a: &Value) -> Option<InboundTurn> {
        if a["type"] != "message" {
            return None;
        }
        let text = |v: &Value| v.as_str().map(str::to_owned);
        let message_id = text(&a["id"])?;
        let conversation_id = text(&a["conversation"]["id"])?;
        let from_id = text(&a["from"]["id"])?;
        if a["recipient"]["id"].as_str() == Some(from_id.as_str()) {
            return None;
        }
        // A 1:1 chat acts as the person. A group chat or a channel thread is shared, so it acts as
        // the group an admin mapped it to (the gateway decides, #830), and only a message that
        // @mentions the bot is a question to it. Anything else — a missing type included — is not
        // evidence of either, and is ignored.
        let kind = match a["conversation"]["conversationType"].as_str() {
            Some("personal") => ConversationKind::Personal,
            Some("groupChat") => ConversationKind::Group,
            Some("channel") => ConversationKind::Channel,
            other => {
                tracing::info!(conversation_type = ?other, "teams: not a known conversation, ignored");
                return None;
            }
        };
        if kind != ConversationKind::Personal && !mentions(a, a["recipient"]["id"].as_str()?) {
            tracing::debug!("teams: a shared-chat message that does not mention the bot, ignored");
            return None;
        }
        let tenant = text(&a["conversation"]["tenantId"])
            .or_else(|| text(&a["channelData"]["tenant"]["id"]));
        if !tenant
            .as_deref()
            .is_some_and(|t| self.cfg.allowed_tenants.iter().any(|a| a == t))
        {
            tracing::warn!(tenant = ?tenant, "teams: sender's tenant is not allowed, ignored");
            return None;
        }
        let conversation = ConversationRef {
            kind,
            id: conversation_id.clone(),
            team_id: (kind == ConversationKind::Channel)
                .then(|| text(&a["channelData"]["team"]["id"]))
                .flatten(),
            channel_id: (kind == ConversationKind::Channel)
                .then(|| text(&a["channelData"]["channel"]["id"]))
                .flatten(),
        };
        // Where the reply goes: a 1:1 chat answers the activity; a group chat posts into the
        // conversation; a channel answers the thread's ROOT, so the reply lands in the thread.
        let (reply_mode, reply_to_id) = match kind {
            ConversationKind::Personal => ("reply", a["id"].clone()),
            ConversationKind::Group => ("post", Value::Null),
            ConversationKind::Channel => {
                ("reply", json!(thread_root(&conversation_id, &message_id)))
            }
        };
        Some(InboundTurn {
            message_id,
            conversation_id,
            conversation,
            user: UserRef {
                id: from_id,
                directory_id: text(&a["from"]["aadObjectId"]),
                directory_tenant: tenant,
                display_name: text(&a["from"]["name"]),
            },
            text: question_text(a, a["recipient"]["id"].as_str()),
            attachments: attachments(a),
            reply_to: json!({
                "serviceUrl": a["serviceUrl"],
                "conversation": a["conversation"],
                "from": a["recipient"],
                "recipient": a["from"],
                "replyToId": reply_to_id,
                "mode": reply_mode,
            }),
        })
    }

    async fn send_reply(&self, turn: &InboundTurn, text: &str) -> Result<(), ChannelError> {
        let r = &turn.reply_to;
        let mut body = json!({
            "type": "message", "from": r["from"], "recipient": r["recipient"],
            "conversation": r["conversation"], "text": text, "textFormat": "markdown",
        });
        // A turn stored before `mode` existed was a 1:1 reply.
        let url = if r["mode"].as_str() == Some("post") {
            conversation_url(r)
        } else {
            body["replyToId"] = r["replyToId"].clone();
            format!(
                "{}/{}",
                conversation_url(r),
                path_segment(r["replyToId"].as_str().unwrap_or_default())
            )
        };
        self.post_activity(url, body).await
    }

    async fn member_profile(&self, turn: &InboundTurn) -> Result<MemberProfile, ChannelError> {
        let url = member_url(
            turn.reply_to["serviceUrl"].as_str().unwrap_or_default(),
            &turn.conversation_id,
            &turn.user.id,
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.connector_token().await?)
            .send()
            .await
            .map_err(|e| ChannelError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ChannelError::Refused {
                status: resp.status().as_u16(),
                body: String::new(),
            });
        }
        let member: Value = resp
            .json()
            .await
            .map_err(|e| ChannelError::Http(e.to_string()))?;
        let member = ConversationMember::from_json(&member);
        Ok(MemberProfile {
            email: member.email,
            upn: member.upn,
        })
    }

    async fn typing(&self, turn: &InboundTurn) -> Result<(), ChannelError> {
        let r = &turn.reply_to;
        let body = json!({
            "type": "typing", "from": r["from"], "recipient": r["recipient"],
            "conversation": r["conversation"],
        });
        self.post_activity(conversation_url(r), body).await
    }

    async fn fetch_attachment(
        &self,
        turn: &InboundTurn,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ChannelError> {
        let service = turn.reply_to["serviceUrl"].as_str().unwrap_or_default();
        let target = crate::fetch::check(&attachment.url, service, &self.cfg.attachment_hosts)
            .await
            .map_err(|e| ChannelError::Http(format!("refused: {e}")))?;
        // Inline content lives on the connector and needs the bot's bearer; a
        // file link is pre-authorised. Never hand the bearer to another host.
        let req = match target {
            crate::fetch::Target::Connector => self
                .http
                .get(&attachment.url)
                .bearer_auth(self.connector_token().await?),
            crate::fetch::Target::Pinned { host, addrs } => {
                client(reqwest::Client::builder().resolve_to_addrs(&host, &addrs))
                    .get(&attachment.url)
            }
        };
        let mut resp = req
            .send()
            .await
            .map_err(|e| ChannelError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ChannelError::Refused {
                status: resp.status().as_u16(),
                body: String::new(),
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ChannelError::Http(e.to_string()))?
        {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > max_bytes {
                return Err(ChannelError::Http(format!("larger than {max_bytes} bytes")));
            }
        }
        Ok(bytes)
    }
}

/// Every request the channel makes: bounded, and redirects are not followed
/// (a redirect would reach a host nobody checked; a 3xx is a refusal).
fn client(builder: reqwest::ClientBuilder) -> reqwest::Client {
    builder
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default()
}

/// `{serviceUrl}/v3/conversations/{id}/activities`.
fn conversation_url(reply_to: &Value) -> String {
    format!(
        "{}/v3/conversations/{}/activities",
        reply_to["serviceUrl"]
            .as_str()
            .unwrap_or_default()
            .trim_end_matches('/'),
        path_segment(reply_to["conversation"]["id"].as_str().unwrap_or_default())
    )
}

/// Whether the activity @mentions `bot` (a `mention` entity naming it).
fn mentions(a: &Value, bot: &str) -> bool {
    a["entities"].as_array().is_some_and(|es| {
        es.iter()
            .any(|e| e["type"] == "mention" && e["mentioned"]["id"].as_str() == Some(bot))
    })
}

/// A channel thread's root activity: Teams names it in the conversation id
/// (`19:…@thread.tacv2;messageid=<root>`); a message with none is its own root.
fn thread_root<'a>(conversation_id: &'a str, message_id: &'a str) -> &'a str {
    conversation_id
        .split(';')
        .find_map(|part| part.strip_prefix("messageid="))
        .filter(|root| !root.is_empty())
        .unwrap_or(message_id)
}

/// The person's question: the message text with the BOT's own mention
/// removed (crew Q-F8). Other mentions stay, as the plain name — in a group
/// chat "<at>escurel</at> ask <at>Bob</at> about Q3" is "ask Bob about Q3".
/// The bot's mention is the text of every `mention` entity naming `bot`
/// (`<at>{name}</at>` when an entity carries no text).
fn question_text(a: &Value, bot: Option<&str>) -> String {
    let mut text = a["text"].as_str().unwrap_or_default().to_owned();
    let own = a["entities"].as_array().into_iter().flatten().filter(|e| {
        e["type"] == "mention" && bot.is_some() && e["mentioned"]["id"].as_str() == bot
    });
    for e in own {
        let tag = e["text"].as_str().map(str::to_owned).or_else(|| {
            e["mentioned"]["name"]
                .as_str()
                .map(|n| format!("<at>{n}</at>"))
        });
        if let Some(tag) = tag.filter(|t| !t.is_empty()) {
            text = text.replace(&tag, "");
        }
    }
    text.replace("<at>", "")
        .replace("</at>", "")
        .trim()
        .to_owned()
}

/// The downloadable attachments of an activity. Teams repeats the message
/// itself as an inline `text/html` attachment with no URL; those are skipped.
fn attachments(a: &Value) -> Vec<Attachment> {
    a["attachments"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|att| {
                    let name = att["name"].as_str().unwrap_or("attachment").to_owned();
                    let declared = att["contentType"].as_str().unwrap_or_default();
                    let (url, content_type) = if declared == TEAMS_FILE {
                        let url = att["content"]["downloadUrl"].as_str()?;
                        (url, mime_for(&name).to_owned())
                    } else {
                        (att["contentUrl"].as_str()?, declared.to_owned())
                    };
                    Some(Attachment {
                        name,
                        content_type,
                        url: url.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A file's MIME type from its extension (a Teams file attachment names only
/// the file type).
fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("pdf") => "application/pdf",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bots_own_mention_is_stripped() {
        let msg = |text: &str, entities: Value| json!({ "text": text, "entities": entities });
        let bot = json!({ "type": "mention", "text": "<at>escurel</at>",
                          "mentioned": { "id": "28:bot", "name": "escurel" } });
        let bob = json!({ "type": "mention", "text": "<at>Bob</at>",
                          "mentioned": { "id": "29:bob", "name": "Bob" } });
        assert_eq!(
            question_text(
                &msg("<at>escurel</at> who renews?", json!([bot])),
                Some("28:bot")
            ),
            "who renews?"
        );
        assert_eq!(
            question_text(
                &msg(
                    "<at>escurel</at> ask <at>Bob</at> about Q3",
                    json!([bot, bob])
                ),
                Some("28:bot")
            ),
            "ask Bob about Q3",
            "a colleague's mention stays, as the name"
        );
        assert_eq!(
            question_text(
                &msg("<at>escurel</at> what about <at>Bob", json!([bot])),
                Some("28:bot")
            ),
            "what about Bob",
            "an unclosed tag drops nothing after it"
        );
        let untexted =
            json!({ "type": "mention", "mentioned": { "id": "28:bot", "name": "escurel" } });
        assert_eq!(
            question_text(
                &msg("<at>escurel</at> hi", json!([untexted])),
                Some("28:bot")
            ),
            "hi"
        );
        assert_eq!(
            question_text(&msg("plain", json!([])), Some("28:bot")),
            "plain"
        );
    }

    #[test]
    fn a_channel_reply_goes_to_the_thread_root() {
        assert_eq!(
            thread_root("19:c@thread.tacv2;messageid=root-1", "m-5"),
            "root-1"
        );
        assert_eq!(thread_root("19:c@thread.tacv2", "m-5"), "m-5");
    }

    #[test]
    fn the_inline_html_copy_is_not_an_attachment_and_a_teams_file_is() {
        let a = json!({ "attachments": [
            { "contentType": "text/html", "content": "<p>hi</p>" },
            { "contentType": TEAMS_FILE, "name": "Q3.pdf",
              "content": { "downloadUrl": "https://files/q3", "fileType": "pdf" } },
        ]});
        let got = attachments(&a);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content_type, "application/pdf");
        assert_eq!(got[0].url, "https://files/q3");
    }
}
