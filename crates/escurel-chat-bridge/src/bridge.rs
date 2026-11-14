//! A channel turn → an escurel page-chat turn → one reply.
//!
//! Each chat on a platform is a `conversation` instance its subject owns
//! (`teams-<hash>`), and its page-bound chat group
//! (`page-chat:v1:<sha256(subject)>:<page>`) is where the turn and the answer
//! live — the shape the runner answers in chat mode. Everything is done FOR
//! THE TURN'S SUBJECT, with a short-lived token the GATEWAY mints
//! (`mint_user_token`, #830) for the bridge's own `escurel:bridge` credential:
//! in a 1:1 chat the subject is the person, with the groups the gateway
//! resolved from the organisation's group directories; in a group chat or a
//! channel it is the group principal (`group:<g>`) an admin mapped the
//! conversation to, the person recorded as the asker. The gateway bounds the
//! token (≤ 900 s, the bridge's few tools) and stamps `captured_via:
//! bridge:msteams` on the event; the run executes with exactly that authority,
//! and a page it cannot read never reaches it. A chat the gateway declines
//! (unmapped, the sender not in the group, the directory unreachable) gets one
//! polite reply and no run.
//!
//! DEPRECATED, for one release: with no gateway credential but a signing key
//! (`ESCUREL_BRIDGE_AUTH_*`) the bridge signs the person's token itself — no
//! groups, 1:1 chats only.
//!
//! Every step is idempotent, so a restarted bridge replays an unfinished turn
//! from the top: the page create is a create-if-absent, the user message has
//! a fixed `msg_id`, the event a fixed `event_id`, and the runner answers one
//! `(chat_group_id, user_msg_id)` once.
//!
//! A turn has a durable deadline: its admission time plus the reply timeout.
//! A gateway outage is retried with capped backoff until then — and a
//! periodic sweep re-works any open turn nothing is working on — so the
//! person gets the answer once the gateway is back, or, at the deadline, the
//! timeout notice (sent through the platform, so it needs no gateway). Both
//! go through [`Bridge::reply`], which sends at most once.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use escurel_auth::Signer;
use escurel_client::{
    AppendMessageRequest, CaptureEventRequest, Client, ListMessagesRequest, SecretString,
    UpdatePageRequest,
};
use escurel_types::chat_channel::ConversationKind;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::channel::{ChatChannel, InboundTurn, short_hash};
use crate::config::{BridgeConfig, GatewayAuth};
use crate::store::{OpenTurn, Store, TurnState};

/// Sent when no answer arrived within the reply timeout.
const TIMED_OUT: &str = "I couldn't get an answer in time. Please ask again.";
const SEND_ATTEMPTS: u32 = 3;
/// The longest wait between two tries at a turn.
const MAX_WORK_BACKOFF: Duration = Duration::from_secs(30);
/// How often open turns nothing is working on are picked up again.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// How many times a turn asks Teams who the sender is before telling them it
/// cannot check their access (crew U-F6).
const PROFILE_ATTEMPTS: u32 = 3;

/// Why a turn is not answered: the person is told once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decline {
    /// A shared chat no `chat-channel` page maps (or one that cannot be mapped).
    Unmapped,
    /// Two mappings name different groups.
    Ambiguous,
    /// The sender is not a member of the mapped group.
    NotMember,
    /// The group directory (or Teams' member profile) cannot be reached.
    Unavailable,
    /// The gateway has no group directory, so it answers 1:1 chats only.
    NoDirectory,
    /// The deprecated self-signed bridge answers 1:1 chats only.
    OneToOneOnly,
    /// The bridge or the gateway is set up wrong: asking again cannot help.
    /// The gateway's code, logged at ERROR with what to fix.
    Misconfigured(&'static str),
}

impl Decline {
    fn from_code(code: &str) -> Option<Self> {
        use escurel_types::chat_channel::mint_codes as c;
        Some(match code {
            c::UNMAPPED => Self::Unmapped,
            c::AMBIGUOUS => Self::Ambiguous,
            c::NOT_MEMBER => Self::NotMember,
            c::DIRECTORY_UNAVAILABLE => Self::Unavailable,
            c::NO_DIRECTORY => Self::NoDirectory,
            c::IDENTITY_MISMATCH => Self::Misconfigured(c::IDENTITY_MISMATCH),
            c::TENANT_NOT_SERVED => Self::Misconfigured(c::TENANT_NOT_SERVED),
            c::UNSUPPORTED => Self::Misconfigured(c::UNSUPPORTED),
            c::BRIDGE_REQUIRED => Self::Misconfigured(c::BRIDGE_REQUIRED),
            "admin_required" => Self::Misconfigured("admin_required"),
            "agent_scope_denied" => Self::Misconfigured("agent_scope_denied"),
            _ => return None,
        })
    }

    /// What an operator changes for a [`Self::Misconfigured`] code.
    fn fix(code: &str) -> &'static str {
        use escurel_types::chat_channel::mint_codes as c;
        match code {
            c::UNSUPPORTED => {
                "the gateway does not mint for a bridge: set ESCUREL_AUTH_BRIDGE_PRINCIPAL_TEMPLATE \
                 (and ESCUREL_AUTH_SIGNING_KEY) on it"
            }
            c::TENANT_NOT_SERVED => {
                "ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS lists a tenant the gateway's \
                 ESCUREL_AUTH_GRAPH_TENANT_ID does not serve"
            }
            c::IDENTITY_MISMATCH => {
                "Teams' member profile names an email Graph does not know this sender by"
            }
            _ => {
                "ESCUREL_BRIDGE_GATEWAY_TOKEN[_FILE] must be the bridge's own credential with the \
                 escurel:bridge role (ESCUREL_AUTH_BRIDGE_ROLE_VALUE)"
            }
        }
    }

    /// The one reply the person gets.
    fn notice(self, turn: &InboundTurn) -> String {
        match self {
            Self::Unmapped => match turn.conversation.mapping_hint() {
                Some(ids) => format!(
                    "I can't answer in this chat yet: it isn't connected to an escurel group. \
                     An escurel admin can connect it with a `chat-channel` page naming {ids} and \
                     the group it should act as."
                ),
                None => "I can't answer in this channel: Teams didn't tell me which team and \
                    channel it is, so it can't be connected to an escurel group."
                    .to_owned(),
            },
            Self::Ambiguous => "I can't answer in this chat: it is connected to more than one \
                 escurel group. An escurel admin needs to keep just one `chat-channel` page for it."
                .to_owned(),
            Self::NotMember => "I can't answer for you here: you're not a member of the escurel \
                 group this chat is connected to."
                .to_owned(),
            Self::Unavailable => {
                "I can't check your access right now. Please try again in a few minutes.".to_owned()
            }
            Self::NoDirectory => "I can't answer in this chat: this bridge has no group \
                 directory, so it answers shared chats only when one is configured. You can ask \
                 me 1:1."
                .to_owned(),
            Self::OneToOneOnly => {
                "I only answer 1:1 chats on this bridge. Please message me directly.".to_owned()
            }
            Self::Misconfigured(_) => "I can't answer right now: this bot isn't set up \
                 correctly. Please tell your escurel admin."
                .to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("no escurel principal for this user: {0}")]
    NoPrincipal(String),
    #[error("the gateway declined the turn: {0:?}")]
    Declined(Decline),
    #[error("signing the asker's token: {0}")]
    Sign(String),
    #[error("mint_user_token: {0}")]
    Mint(String),
    #[error("the sender's Teams member profile: {0}")]
    Profile(String),
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
    /// The deprecated self-signed path's key; `None` when the gateway mints.
    signer: Option<Signer>,
    cfg: BridgeConfig,
    /// The client on the bridge's own credential, rebuilt when it rotates.
    service: tokio::sync::Mutex<Option<(String, Client)>>,
    /// Turn keys a task is working on now, so a sweep never doubles one.
    working: Mutex<HashSet<String>>,
}

impl<C: ChatChannel> Bridge<C> {
    #[must_use]
    pub fn new(cfg: BridgeConfig, channel: C, store: Store, signer: Option<Signer>) -> Arc<Self> {
        Arc::new(Self {
            channel,
            store,
            signer,
            cfg,
            service: tokio::sync::Mutex::new(None),
            working: Mutex::new(HashSet::new()),
        })
    }

    #[must_use]
    pub fn channel(&self) -> &C {
        &self.channel
    }

    /// The JWKS a gateway trusts the bridge's self-signed tokens through: the
    /// public half of its signing key, under its kid
    /// (`GET /.well-known/jwks.json`). Empty when the gateway mints.
    #[must_use]
    pub fn jwks(&self) -> Value {
        json!({ "keys": self.signer.iter().map(Signer::public_jwk).collect::<Vec<_>>() })
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
        let Some(admitted_ms) = self.store.admit(self.channel.name(), &turn)? else {
            tracing::info!(message_id = %turn.message_id, "chat bridge: replayed message, ignored");
            return Ok(());
        };
        self.spawn(OpenTurn {
            turn,
            state: TurnState::Received,
            admitted_ms,
        });
        Ok(())
    }

    /// Pick up the open turns nothing is working on: at boot, the ones a
    /// previous process left; on each sweep, any whose task ended.
    ///
    /// # Errors
    /// The state store cannot be read.
    pub fn resume(self: &Arc<Self>) -> Result<usize, BridgeError> {
        let open = self.store.open_turns(self.channel.name())?;
        let mut n = 0;
        for turn in open {
            n += usize::from(self.spawn(turn));
        }
        Ok(n)
    }

    /// Re-run [`Self::resume`] every [`SWEEP_INTERVAL`] for as long as the
    /// bridge lives.
    pub fn start_sweep(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match this.resume() {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(resumed = n, "chat bridge: sweep picked up open turns"),
                    Err(e) => tracing::warn!(error = %e, "chat bridge: sweep could not read turns"),
                }
            }
        });
    }

    /// The moment `open`'s reply is due: admission plus the reply timeout.
    fn deadline(&self, open: &OpenTurn) -> Instant {
        let elapsed = u64::try_from(crate::store::now_ms().saturating_sub(open.admitted_ms))
            .map(Duration::from_millis)
            .unwrap_or_default();
        Instant::now() + self.cfg.reply_timeout.saturating_sub(elapsed)
    }

    /// Work a turn in the background until it is answered or due. A gateway
    /// outage is retried with capped backoff (every step is idempotent); at
    /// the deadline the person is told it timed out. A person the template
    /// cannot map is closed, so nothing retries them forever. `false` when a
    /// task already works this turn.
    fn spawn(self: &Arc<Self>, open: OpenTurn) -> bool {
        let id = open.turn.key();
        if !self
            .working
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone())
        {
            return false;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut open = open;
            let deadline = this.deadline(&open);
            let mut backoff = this.cfg.poll_interval;
            let mut attempt = 0u32;
            let mut memory = TurnMemory::default();
            loop {
                attempt += 1;
                if Instant::now() >= deadline {
                    this.time_out(&open.turn).await;
                    break;
                }
                match this.work(&mut open, deadline, &mut memory).await {
                    Ok(()) => break,
                    Err(e @ BridgeError::NoPrincipal(_)) => {
                        tracing::warn!(message_id = %id, error = %e, "chat bridge: not answered");
                        let _ = this
                            .store
                            .close(this.channel.name(), &id, TurnState::Unmapped);
                        break;
                    }
                    // Only a turn not yet in escurel is declined: once its question is captured
                    // a later refusal (a re-mint during an outage) must not drop the answer
                    // (crew Q-F2) — it is retried like any gateway error until the deadline.
                    Err(BridgeError::Declined(why)) if open.state == TurnState::Received => {
                        if let Decline::Misconfigured(code) = why {
                            tracing::error!(message_id = %id, code, fix = Decline::fix(code),
                                "chat bridge: the gateway refuses this bridge's turns; fix the \
                                 configuration");
                        } else {
                            tracing::info!(message_id = %id, decline = ?why,
                                "chat bridge: the gateway declined the turn; telling the person once");
                        }
                        let notice = why.notice(&open.turn);
                        if let Err(e) = this
                            .reply(&open.turn, "declined", &notice, TurnState::Declined)
                            .await
                        {
                            tracing::error!(message_id = %id, error = %e,
                                "chat bridge: could not record the decline");
                        }
                        break;
                    }
                    Err(e) => {
                        tracing::error!(message_id = %id, attempt, error = %e,
                            "chat bridge: turn failed; retrying");
                        let wait = backoff.min(deadline.saturating_duration_since(Instant::now()));
                        tokio::time::sleep(wait).await;
                        backoff = (backoff * 2).min(MAX_WORK_BACKOFF);
                    }
                }
            }
            this.working
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
        });
        true
    }

    /// The turn is due with no answer: say so, once.
    async fn time_out(&self, turn: &InboundTurn) {
        tracing::warn!(message_id = %turn.message_id, "chat bridge: no answer in time");
        if let Err(e) = self
            .reply(turn, "timeout", TIMED_OUT, TurnState::Expired)
            .await
        {
            tracing::error!(message_id = %turn.message_id, error = %e,
                "chat bridge: could not record the timeout");
        }
    }

    async fn work(
        &self,
        open: &mut OpenTurn,
        deadline: Instant,
        memory: &mut TurnMemory,
    ) -> Result<(), BridgeError> {
        let inbound = open.turn.clone();
        let turn = &inbound;
        let name = self.channel.name();
        let mut asker = AskerClient::new(self, turn, memory);
        let (client, principal) = asker.get().await?;
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
            open.state = TurnState::Captured;
            tracing::info!(message_id = %turn.message_id, event_id = %event.event_id,
                "chat bridge: turn captured");
        }
        self.follow(&mut asker, turn, &group, &user_msg_id, deadline)
            .await
    }

    /// Wait for the run's assistant turn, showing typing meanwhile, and send
    /// it as the one reply.
    async fn follow(
        &self,
        asker: &mut AskerClient<'_, C>,
        turn: &InboundTurn,
        group: &str,
        user_msg_id: &str,
        deadline: Instant,
    ) -> Result<(), BridgeError> {
        let mut next_typing = Instant::now();
        while Instant::now() < deadline {
            if let Some((key, text)) = self
                .answer(&asker.get().await?.0, group, user_msg_id)
                .await?
            {
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
        self.time_out(turn).await;
        Ok(())
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

/// What a turn learned across its retries: who the sender is (Teams' member
/// profile) and how often asking failed.
#[derive(Default)]
struct TurnMemory {
    profile: Option<crate::channel::MemberProfile>,
    profile_failures: u32,
}

/// The turn's client, re-minted before its token gets old, and the subject
/// it acts as (the person, or a group principal in a shared chat).
struct AskerClient<'a, C: ChatChannel> {
    bridge: &'a Bridge<C>,
    turn: &'a InboundTurn,
    memory: &'a mut TurnMemory,
    current: Option<(Client, String, Instant)>,
}

impl<'a, C: ChatChannel> AskerClient<'a, C> {
    fn new(bridge: &'a Bridge<C>, turn: &'a InboundTurn, memory: &'a mut TurnMemory) -> Self {
        Self {
            bridge,
            turn,
            memory,
            current: None,
        }
    }

    async fn get(&mut self) -> Result<(Client, String), BridgeError> {
        let ttl = self.bridge.cfg.gateway.token_ttl_secs;
        let refresh_after = Duration::from_secs(ttl / 2);
        // The current token is still good for a while after it is due a refresh.
        let usable_for = Duration::from_secs(ttl * 3 / 4);
        if let Some((client, subject, at)) = &self.current
            && at.elapsed() < refresh_after
        {
            return Ok((client.clone(), subject.clone()));
        }
        let fresh = match &self.bridge.cfg.gateway.auth {
            GatewayAuth::Service { credential } => self.minted(credential).await,
            GatewayAuth::SelfSigned {
                principal_template, ..
            } => self.self_signed(principal_template, ttl),
        };
        let (token, subject) = match (fresh, &self.current) {
            (Ok(fresh), _) => fresh,
            // A refresh that fails (an outage, a directory that cannot answer) keeps the token in
            // hand until it is about to lapse: the turn's answer must not be lost to it (crew Q-F2).
            (Err(e), Some((client, subject, at))) if at.elapsed() < usable_for => {
                tracing::warn!(message_id = %self.turn.message_id, error = %e,
                    "chat bridge: re-minting the turn's token failed; keeping the current one");
                return Ok((client.clone(), subject.clone()));
            }
            (Err(e), _) => return Err(e),
        };
        let client =
            Client::connect(&self.bridge.cfg.gateway.url, SecretString::from(token)).await?;
        self.current = Some((client.clone(), subject.clone(), Instant::now()));
        Ok((client, subject))
    }

    /// Who the sender is beyond the message (their email / UPN, for the
    /// email-keyed directories), asked once per turn. A transient failure is
    /// retried with the turn; a refusal, or [`PROFILE_ATTEMPTS`] failures,
    /// tells the person their access cannot be checked — never a silent
    /// "not a member" from a lookup that had no email (crew U-F6).
    async fn profile(&mut self) -> Result<crate::channel::MemberProfile, BridgeError> {
        if let Some(p) = &self.memory.profile {
            return Ok(p.clone());
        }
        match self.bridge.channel.member_profile(self.turn).await {
            Ok(p) => {
                self.memory.profile = Some(p.clone());
                Ok(p)
            }
            Err(e) => {
                self.memory.profile_failures += 1;
                let refused = matches!(&e,
                    crate::channel::ChannelError::Refused { status, .. }
                        if (400..500).contains(status) && *status != 429);
                if refused || self.memory.profile_failures >= PROFILE_ATTEMPTS {
                    tracing::warn!(error = %e, attempts = self.memory.profile_failures,
                        "chat bridge: the sender's member profile is unavailable");
                    Err(BridgeError::Declined(Decline::Unavailable))
                } else {
                    Err(BridgeError::Profile(e.to_string()))
                }
            }
        }
    }

    /// The client on the bridge's own credential: one per credential value,
    /// rebuilt when the credential rotates.
    async fn service(&self, credential: &str) -> Result<Client, BridgeError> {
        let mut slot = self.bridge.service.lock().await;
        if let Some((held, client)) = slot.as_ref()
            && held == credential
        {
            return Ok(client.clone());
        }
        let client = Client::connect(
            &self.bridge.cfg.gateway.url,
            SecretString::from(credential.to_owned()),
        )
        .await?;
        *slot = Some((credential.to_owned(), client.clone()));
        Ok(client)
    }

    /// Ask the gateway for the turn's token (`mint_user_token`) on the
    /// bridge's own credential. A rejected credential read from a file is
    /// re-read once — a rotation needs no restart.
    async fn minted(
        &mut self,
        credential: &crate::config::ServiceCredential,
    ) -> Result<(String, String), BridgeError> {
        let turn = self.turn;
        let Some(aad) = turn.user.directory_id.as_deref().filter(|v| !v.is_empty()) else {
            return Err(BridgeError::NoPrincipal(
                "the sender has no directory object id".to_owned(),
            ));
        };
        let profile = self.profile().await?;
        let args = json!({
            "channel": self.bridge.channel.name(),
            "tenant_id": turn.user.directory_tenant,
            "aad_object_id": aad,
            "user_id": turn.user.id,
            "email": profile.email,
            "upn": profile.upn,
            "conversation": turn.conversation,
        });
        let mut reloaded = false;
        let minted = loop {
            let service = self.service(&credential.current()).await?;
            match service.call_raw("mint_user_token", args.clone()).await {
                Err(escurel_client::Error::Http { status: 401, .. })
                    if !reloaded && credential.reload() =>
                {
                    tracing::info!(
                        "chat bridge: the gateway rejected the bridge credential; \
                        retrying with the rotated one"
                    );
                    reloaded = true;
                }
                other => break other,
            }
        }
        .map_err(|e| match &e {
            escurel_client::Error::JsonRpc {
                data_code: Some(code),
                message,
                ..
            } => match Decline::from_code(code) {
                Some(d) => BridgeError::Declined(d),
                None if code == escurel_types::chat_channel::mint_codes::NO_PRINCIPAL => {
                    BridgeError::NoPrincipal(message.clone())
                }
                None => BridgeError::Gateway(e),
            },
            _ => BridgeError::Gateway(e),
        })?;
        let text = |k: &str| {
            minted[k]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        match (text("token"), text("subject")) {
            (Some(token), Some(subject)) => Ok((token, subject)),
            _ => Err(BridgeError::Mint(
                "the gateway answered without a token or subject".to_owned(),
            )),
        }
    }

    /// The deprecated path: sign the person's token with the bridge's own key.
    /// No groups; a shared chat is told once that only 1:1 chats are answered.
    fn self_signed(&self, template: &str, ttl: u64) -> Result<(String, String), BridgeError> {
        let turn = self.turn;
        if turn.conversation.kind != ConversationKind::Personal {
            return Err(BridgeError::Declined(Decline::OneToOneOnly));
        }
        let principal = escurel_types::bridge::principal_for(
            template,
            turn.user.directory_id.as_deref(),
            Some(turn.user.id.as_str()),
            turn.user.directory_tenant.as_deref(),
        )
        .map_err(|why| BridgeError::NoPrincipal(format!("template `{template}`: {why}")))?;
        let signer = self
            .bridge
            .signer
            .as_ref()
            .ok_or_else(|| BridgeError::Sign("no signing key".to_owned()))?;
        let token = signer
            .mint_bridge(&principal, self.bridge.channel.name(), ttl)
            .map_err(|e| BridgeError::Sign(e.to_string()))?;
        Ok((token, principal))
    }
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
