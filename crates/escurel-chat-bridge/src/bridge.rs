//! A channel turn → an escurel page-chat turn → one reply.
//!
//! Each person's chat on a platform is a `conversation` instance they own
//! (`teams-<hash>`), and its page-bound chat group
//! (`page-chat:v1:<sha256(asker)>:<page>`) is where the turn and the answer
//! live — the shape the runner answers in chat mode. Everything is done AS
//! THE ASKER: the bridge signs a short-lived token for the mapped principal
//! with the key the gateway trusts (the runner's minted-mode trust), so the
//! event reads `captured_by: <asker>` with no `captured_via`, the run executes
//! with the asker's rights, and a page they cannot read never reaches it.
//!
//! Every step is idempotent, so a restarted bridge replays an unfinished turn
//! from the top: the page create is a create-if-absent, the user message has
//! a fixed `msg_id`, the event a fixed `event_id`, and the runner answers one
//! `(chat_group_id, user_msg_id)` once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use escurel_auth::Signer;
use escurel_client::{
    AppendMessageRequest, CaptureEventRequest, Client, ListMessagesRequest, SecretString,
    UpdatePageRequest,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::channel::{ChatChannel, InboundTurn, short_hash};
use crate::config::BridgeConfig;
use crate::store::{OpenTurn, Store, TurnState};

/// Sent when no answer arrived within the reply timeout.
const TIMED_OUT: &str = "I couldn't get an answer in time. Please ask again.";
const SEND_ATTEMPTS: u32 = 3;
/// Tries at a turn before it waits for the next boot.
const WORK_ATTEMPTS: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("no escurel principal for this user (template `{0}`)")]
    NoPrincipal(String),
    #[error("signing the asker's token: {0}")]
    Sign(String),
    #[error("gateway: {0}")]
    Gateway(#[from] escurel_client::Error),
    #[error("conversation page refused: {0}")]
    Page(String),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

pub struct Bridge<C: ChatChannel> {
    channel: C,
    store: Store,
    signer: Signer,
    cfg: BridgeConfig,
}

impl<C: ChatChannel> Bridge<C> {
    #[must_use]
    pub fn new(cfg: BridgeConfig, channel: C, store: Store, signer: Signer) -> Arc<Self> {
        Arc::new(Self {
            channel,
            store,
            signer,
            cfg,
        })
    }

    #[must_use]
    pub fn channel(&self) -> &C {
        &self.channel
    }

    /// Take a verified payload: admit its turn once and work it in the
    /// background (the platform wants its webhook answered within seconds).
    ///
    /// # Errors
    /// The state store refuses the write.
    pub fn accept(self: &Arc<Self>, payload: &Value) -> Result<(), BridgeError> {
        let Some(turn) = self.channel.to_capture(payload) else {
            return Ok(());
        };
        if !self.store.admit(self.channel.name(), &turn)? {
            tracing::info!(message_id = %turn.message_id, "chat bridge: replayed message, ignored");
            return Ok(());
        }
        self.spawn(OpenTurn {
            turn,
            state: TurnState::Received,
        });
        Ok(())
    }

    /// Pick up the turns a previous process left unanswered.
    ///
    /// # Errors
    /// The state store cannot be read.
    pub fn resume(self: &Arc<Self>) -> Result<usize, BridgeError> {
        let open = self.store.open_turns(self.channel.name())?;
        let n = open.len();
        for turn in open {
            self.spawn(turn);
        }
        Ok(n)
    }

    /// Work a turn in the background. A gateway hiccup is retried (every
    /// step is idempotent); a person the template cannot map is closed, so
    /// a restart does not retry them forever. Whatever is still open after
    /// the last attempt is picked up again by the next boot's [`Self::resume`].
    fn spawn(self: &Arc<Self>, open: OpenTurn) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let id = open.turn.key();
            for attempt in 1..=WORK_ATTEMPTS {
                match this.work(open.clone()).await {
                    Ok(()) => return,
                    Err(e @ BridgeError::NoPrincipal(_)) => {
                        tracing::warn!(message_id = %id, error = %e, "chat bridge: not answered");
                        let _ = this
                            .store
                            .close(this.channel.name(), &id, TurnState::Unmapped);
                        return;
                    }
                    Err(e) => {
                        tracing::error!(message_id = %id, attempt, error = %e,
                            "chat bridge: turn failed");
                        tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    }
                }
            }
        });
    }

    async fn work(&self, open: OpenTurn) -> Result<(), BridgeError> {
        let turn = &open.turn;
        let name = self.channel.name();
        let principal = principal_for(&self.cfg.principal_template, turn)
            .ok_or_else(|| BridgeError::NoPrincipal(self.cfg.principal_template.clone()))?;
        let mut asker = AskerClient::new(self, &principal);
        let page_id = format!(
            "markdown/instances/{}/{}.md",
            self.cfg.conversation_skill,
            conversation_instance_id(name, &turn.conversation_id, &principal)
        );
        let group = format!(
            "page-chat:v1:{:x}:{page_id}",
            Sha256::digest(principal.as_bytes())
        );
        let key = turn.key();
        let user_msg_id = format!("{name}:{key}");

        if open.state == TurnState::Received {
            let client = asker.get().await?;
            self.ensure_conversation(&client, &page_id, &principal)
                .await?;
            let content = self.with_attachments(&client, turn).await;
            client
                .append_message(AppendMessageRequest {
                    chat_group_id: group.clone(),
                    role: "user".to_owned(),
                    content,
                    msg_id: user_msg_id.clone(),
                    metadata: json!({ "channel": name, "conversation_id": turn.conversation_id }),
                    embed: true,
                    ..Default::default()
                })
                .await?;
            let event = client
                .capture_event(CaptureEventRequest {
                    event_id: format!("{name}-{key}"),
                    source: name.to_owned(),
                    label_skill: self.cfg.chat_skill.clone(),
                    instance_page_id: page_id.clone(),
                    title: "chat turn".to_owned(),
                    provenance: json!({
                        "chat_group_id": group,
                        "user_msg_id": user_msg_id,
                        "channel": { "name": name, "conversation_id": turn.conversation_id,
                                     "message_id": turn.message_id },
                    }),
                    ..Default::default()
                })
                .await?;
            self.store.captured(name, &key, &event.event_id)?;
            tracing::info!(message_id = %turn.message_id, event_id = %event.event_id,
                "chat bridge: turn captured");
        }
        self.follow(&mut asker, turn, &group, &user_msg_id).await
    }

    /// Wait for the run's assistant turn, showing typing meanwhile, and send
    /// it as the one reply.
    async fn follow(
        &self,
        asker: &mut AskerClient<'_, C>,
        turn: &InboundTurn,
        group: &str,
        user_msg_id: &str,
    ) -> Result<(), BridgeError> {
        let deadline = Instant::now() + self.cfg.reply_timeout;
        let mut next_typing = Instant::now();
        while Instant::now() < deadline {
            if let Some((key, text)) = self.answer(&asker.get().await?, group, user_msg_id).await? {
                return self.reply(turn, &key, &text, TurnState::Replied).await;
            }
            if Instant::now() >= next_typing {
                if let Err(e) = self.channel.typing(turn).await {
                    tracing::warn!(error = %e, "chat bridge: typing indicator failed");
                }
                next_typing = Instant::now() + self.cfg.typing_interval;
            }
            tokio::time::sleep(self.cfg.poll_interval).await;
        }
        tracing::warn!(message_id = %turn.message_id, "chat bridge: no answer in time");
        self.reply(turn, "timeout", TIMED_OUT, TurnState::Expired)
            .await
    }

    /// The assistant turn answering `user_msg_id`: its `msg_id` (the reply
    /// key, `run:<id>:reply`) and text.
    async fn answer(
        &self,
        client: &Client,
        group: &str,
        user_msg_id: &str,
    ) -> Result<Option<(String, String)>, BridgeError> {
        let recent = client
            .list_messages(ListMessagesRequest {
                chat_group_id: group.to_owned(),
                limit: 50,
                direction: "desc".to_owned(),
                ..Default::default()
            })
            .await?;
        Ok(recent
            .messages
            .into_iter()
            .find(|m| {
                m.role == "assistant"
                    && m.metadata
                        .as_ref()
                        .and_then(|md| md["user_msg_id"].as_str())
                        == Some(user_msg_id)
            })
            .map(|m| (m.msg_id, m.content)))
    }

    /// Send `text` at most once: claim the turn's reply under `key`, post,
    /// close. Only a failure that provably sent nothing is retried; one that
    /// may have reached the platform closes as `send_uncertain`, because a
    /// second post could be a second answer.
    async fn reply(
        &self,
        turn: &InboundTurn,
        key: &str,
        text: &str,
        closed: TurnState,
    ) -> Result<(), BridgeError> {
        let name = self.channel.name();
        let turn_key = turn.key();
        if !self.store.claim_reply(name, &turn_key, key)? {
            return Ok(());
        }
        let mut attempt = 1;
        let outcome = loop {
            match self.channel.send_reply(turn, text).await {
                Ok(()) => {
                    tracing::info!(message_id = %turn.message_id, reply_key = key,
                        "chat bridge: reply sent");
                    break closed;
                }
                Err(e) if e.never_sent() && attempt < SEND_ATTEMPTS => {
                    tracing::warn!(attempt, error = %e, "chat bridge: reply not sent, retrying");
                    tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
                    attempt += 1;
                }
                Err(e) if e.maybe_delivered() => {
                    tracing::warn!(message_id = %turn.message_id, reply_key = key, error = %e,
                        "chat bridge: the reply may have been delivered; not sending it again");
                    break TurnState::SendUncertain;
                }
                Err(e) => {
                    tracing::error!(message_id = %turn.message_id, error = %e,
                        "chat bridge: sending the reply failed");
                    break TurnState::SendFailed;
                }
            }
        };
        self.store.close(name, &turn_key, outcome)?;
        Ok(())
    }

    /// Create the asker's conversation page for this chat unless it exists.
    async fn ensure_conversation(
        &self,
        client: &Client,
        page_id: &str,
        principal: &str,
    ) -> Result<(), BridgeError> {
        let id = page_id
            .rsplit('/')
            .next()
            .and_then(|f| f.strip_suffix(".md"))
            .unwrap_or_default();
        let title = format!("{} chat", self.channel.name());
        let content = format!(
            "---\nkind: instance\nskill: {skill}\nid: {id}\ntitle: {title}\nowner: {owner}\n\
             started_at: {now}\n---\n# {plain}\n",
            skill = self.cfg.conversation_skill,
            title = json!(title),
            owner = json!(principal),
            now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            plain = title,
        );
        let resp = client
            .update_page(UpdatePageRequest {
                page_id: page_id.to_owned(),
                content,
                // Create-if-absent: a page already there answers `conflict`.
                base_sha256: Some(String::new()),
                ..Default::default()
            })
            .await?;
        let exists = resp.head_sha256.as_deref().is_some_and(|h| !h.is_empty());
        if resp.ok || exists {
            return Ok(());
        }
        Err(BridgeError::Page(
            resp.issues
                .iter()
                .map(|i| format!("{}: {}", i.code, i.message))
                .collect::<Vec<_>>()
                .join("; "),
        ))
    }

    /// The person's text, plus one line per attachment: the page it was
    /// ingested as, or why it was not.
    async fn with_attachments(&self, client: &Client, turn: &InboundTurn) -> String {
        if turn.attachments.is_empty() {
            return turn.text.clone();
        }
        let mut lines = vec![turn.text.clone(), String::new(), "Attachments:".to_owned()];
        for (i, att) in turn.attachments.iter().enumerate() {
            let outcome = match self
                .channel
                .fetch_attachment(turn, att, self.cfg.attachment_max_bytes)
                .await
            {
                Err(e) => Err(format!("download failed: {e}")),
                Ok(bytes) => {
                    ingest(
                        client,
                        &att.content_type,
                        &att.name,
                        &bytes,
                        &format!(
                            "{}-{}",
                            self.channel.name(),
                            short_hash(&[&turn.key(), &i.to_string()])
                        ),
                    )
                    .await
                }
            };
            lines.push(match outcome {
                Ok(page) => format!(
                    "- {} ({}): ingested as `{page}`",
                    att.name, att.content_type
                ),
                Err(why) => format!("- {} ({}): not ingested: {why}", att.name, att.content_type),
            });
        }
        lines.join("\n")
    }
}

/// Upload through the gateway's document intake (`ingest_upload`, idempotent
/// on `event_id`); the materialised page, or why there is none.
async fn ingest(
    client: &Client,
    content_type: &str,
    name: &str,
    bytes: &[u8],
    event_id: &str,
) -> Result<String, String> {
    let out = client
        .call_raw(
            "ingest_upload",
            json!({
                "content_type": content_type,
                "bytes_b64": base64::engine::general_purpose::STANDARD.encode(bytes),
                "title": name,
                "event_id": event_id,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    if let Some(page) = out["page_id"].as_str().filter(|p| !p.is_empty()) {
        return Ok(page.to_owned());
    }
    let status = out["status"].as_str().unwrap_or("refused");
    let detail = out["issues"][0]["message"]
        .as_str()
        .or_else(|| out["issue"].as_str())
        .unwrap_or("the gateway has no document skill for this type");
    Err(format!("{status}: {detail}"))
}

/// The asker's client, re-signed before its token gets old.
struct AskerClient<'a, C: ChatChannel> {
    bridge: &'a Bridge<C>,
    principal: String,
    current: Option<(Client, Instant)>,
}

impl<'a, C: ChatChannel> AskerClient<'a, C> {
    fn new(bridge: &'a Bridge<C>, principal: &str) -> Self {
        Self {
            bridge,
            principal: principal.to_owned(),
            current: None,
        }
    }

    async fn get(&mut self) -> Result<Client, BridgeError> {
        let ttl = self.bridge.cfg.gateway.token_ttl_secs;
        let refresh_after = Duration::from_secs(ttl / 2);
        if let Some((client, at)) = &self.current
            && at.elapsed() < refresh_after
        {
            return Ok(client.clone());
        }
        // No groups, no run claims: exactly the person, as their own token
        // would present them.
        let token = self
            .bridge
            .signer
            .mint_scoped(&self.principal, &[], ttl, None)
            .map_err(|e| BridgeError::Sign(e.to_string()))?;
        let client =
            Client::connect(&self.bridge.cfg.gateway.url, SecretString::from(token)).await?;
        self.current = Some((client.clone(), Instant::now()));
        Ok(client)
    }
}

/// The escurel subject for a platform user: the template with
/// `{aadObjectId}`, `{userId}` and `{tenantId}` filled in. `None` when the
/// template names a field the user lacks — never a guessed identity.
fn principal_for(template: &str, turn: &InboundTurn) -> Option<String> {
    let mut out = template.to_owned();
    for (field, value) in [
        ("{aadObjectId}", turn.user.directory_id.as_deref()),
        ("{userId}", Some(turn.user.id.as_str())),
        ("{tenantId}", turn.user.directory_tenant.as_deref()),
    ] {
        if out.contains(field) {
            out = out.replace(field, value.filter(|v| !v.is_empty())?);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `teams-<32 hex>`: one conversation page per (channel, chat, person).
fn conversation_instance_id(channel: &str, conversation_id: &str, principal: &str) -> String {
    let prefix = if channel == "msteams" {
        "teams"
    } else {
        channel
    };
    format!(
        "{prefix}-{}",
        short_hash(&[channel, conversation_id, principal])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::UserRef;

    fn turn(aad: Option<&str>) -> InboundTurn {
        InboundTurn {
            message_id: "m".into(),
            conversation_id: "c".into(),
            user: UserRef {
                id: "29:x".into(),
                directory_id: aad.map(str::to_owned),
                directory_tenant: Some("t".into()),
                display_name: None,
            },
            text: String::new(),
            attachments: vec![],
            reply_to: Value::Null,
        }
    }

    #[test]
    fn the_principal_comes_from_the_template_or_not_at_all() {
        assert_eq!(
            principal_for("user:{aadObjectId}", &turn(Some("oid"))).as_deref(),
            Some("user:oid")
        );
        assert_eq!(
            principal_for("{tenantId}/{userId}", &turn(None)).as_deref(),
            Some("t/29:x")
        );
        assert_eq!(principal_for("user:{aadObjectId}", &turn(None)), None);
    }

    #[test]
    fn one_conversation_page_per_chat_and_person() {
        let a = conversation_instance_id("msteams", "c1", "user:a");
        assert!(
            a.starts_with("teams-") && a.len() == "teams-".len() + 32,
            "{a}"
        );
        assert_eq!(a, conversation_instance_id("msteams", "c1", "user:a"));
        assert_ne!(a, conversation_instance_id("msteams", "c1", "user:b"));
    }
}
