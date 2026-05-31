//! Agent-surface commands (everything that speaks the `Escurel`
//! service). Each handler builds one request, calls one
//! [`escurel_client::Client`] method, and returns the JSON projection.

use std::io::Read as _;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use escurel_client::{
    AppendMessageRequest, AssignEventRequest, CaptureEventRequest, Client, ExpandRequest,
    ListEventsRequest, ListInboxRequest, ListInstancesRequest, ListMessagesRequest,
    ListSkillsRequest, NeighboursRequest, ResolveRequest, RunStoredQueryRequest, SearchRequest,
    UpdatePageRequest, ValidateRequest,
};
use serde_json::{Value, json};

use crate::Command;
use crate::convert::{event, json_or_null, opt, page_ref};

// --- argument groups -----------------------------------------------

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Free-text query.
    pub q: String,
    /// Top-k hits. 0 → server default of 10.
    #[arg(long, default_value_t = 10)]
    pub k: u32,
    /// "skill" | "instance" | "any" (default).
    #[arg(long, default_value = "any")]
    pub page_type: String,
    /// Restrict to one skill.
    #[arg(long)]
    pub skill: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum SkillCmd {
    /// Return the tenant's Tier-1 skill catalogue.
    List,
}

#[derive(Subcommand, Debug)]
pub enum InstanceCmd {
    /// Enumerate instances of a skill.
    List(InstanceListArgs),
}

#[derive(Args, Debug)]
pub struct InstanceListArgs {
    #[arg(long)]
    pub skill: String,
    /// "asc" | "desc"; empty for natural order.
    #[arg(long, default_value = "")]
    pub order_by_at: String,
    /// 0 means no limit.
    #[arg(long, default_value_t = 0)]
    pub limit: u32,
}

#[derive(Subcommand, Debug)]
pub enum PageCmd {
    /// Fetch a page's frontmatter + body + outbound wikilinks.
    Expand { page_id: String },
    /// Dry-run the indexer over a draft body (read from stdin) without
    /// committing.
    Validate { page_id: String },
    /// Upsert a markdown page. Body is read from stdin.
    Update { page_id: String },
}

#[derive(Subcommand, Debug)]
pub enum LinkCmd {
    /// Typed link-graph traversal.
    Neighbours(NeighboursArgs),
}

#[derive(Args, Debug)]
pub struct NeighboursArgs {
    pub page_id: String,
    /// "in" | "out" | "both" (default).
    #[arg(long, default_value = "both")]
    pub direction: String,
    /// Filter to a specific link skill (e.g. "meeting").
    #[arg(long)]
    pub link_skill: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub limit: u32,
}

#[derive(Subcommand, Debug)]
pub enum EventCmd {
    /// Append an event to the global inbox. Body is read from stdin
    /// unless `--body` is given.
    Capture(CaptureArgs),
    /// List unprocessed inbox events.
    Inbox {
        /// 0 means no limit.
        #[arg(long, default_value_t = 0)]
        limit: u32,
    },
    /// List an instance's processed event history.
    List {
        #[arg(long)]
        instance: String,
        /// 0 means no limit.
        #[arg(long, default_value_t = 0)]
        limit: u32,
    },
    /// Bind an inbox event to an instance (→ processed).
    Assign {
        #[arg(long)]
        event: String,
        #[arg(long)]
        instance: String,
    },
}

#[derive(Args, Debug)]
pub struct CaptureArgs {
    #[arg(long, default_value = "manual")]
    pub source: String,
    #[arg(long, default_value = "text/plain")]
    pub mime: String,
    /// Processing skill the event's label links to.
    #[arg(long, default_value = "note")]
    pub label_skill: String,
    #[arg(long)]
    pub title: String,
    /// Event body. If absent, read from stdin.
    #[arg(long)]
    pub body: Option<String>,
    /// Candidate instance to pre-flag (stays in the inbox until
    /// `event assign`).
    #[arg(long)]
    pub instance: Option<String>,
    /// Event time (RFC-3339 UTC). Undated when absent.
    #[arg(long)]
    pub at: Option<String>,
    /// Caller-supplied event id. Server mints a ULID when absent.
    #[arg(long)]
    pub event_id: Option<String>,
    /// Inline JSON provenance value.
    #[arg(long)]
    pub provenance: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum QueryCmd {
    /// Execute a `[[query::<id>]]` instance with named parameters.
    Run(QueryRunArgs),
}

#[derive(Args, Debug)]
pub struct QueryRunArgs {
    pub query_id: String,
    /// JSON object of parameters, e.g. `{"skill":"customer"}`.
    #[arg(long, default_value = "{}")]
    pub params: String,
}

#[derive(Subcommand, Debug)]
pub enum ChatCmd {
    /// Append a message to a chat group. Content is read from stdin
    /// unless `--content` is given.
    Append(ChatAppendArgs),
    /// Read back a chat group's history.
    List(ChatListArgs),
}

#[derive(Args, Debug)]
pub struct ChatAppendArgs {
    #[arg(long, short = 'g')]
    pub group: String,
    /// `user` | `assistant` | `system` | `tool`.
    #[arg(long, default_value = "user")]
    pub role: String,
    /// Message content. If absent, read from stdin.
    #[arg(long)]
    pub content: Option<String>,
    #[arg(long)]
    pub author: Option<String>,
    /// Event time (RFC-3339 UTC). Server stamps `CURRENT_TIMESTAMP`
    /// when absent.
    #[arg(long)]
    pub ts: Option<String>,
    /// Inline JSON metadata.
    #[arg(long)]
    pub metadata: Option<String>,
    /// Caller-supplied message id. Server mints a ULID when absent.
    #[arg(long)]
    pub msg_id: Option<String>,
    /// Skip embedding.
    #[arg(long)]
    pub no_embed: bool,
}

#[derive(Args, Debug)]
pub struct ChatListArgs {
    #[arg(long, short = 'g')]
    pub group: String,
    /// Inclusive lower bound (RFC-3339).
    #[arg(long)]
    pub since: Option<String>,
    /// Exclusive upper bound (RFC-3339).
    #[arg(long)]
    pub until: Option<String>,
    /// 0 → server default (100); hard cap 1000.
    #[arg(long, default_value_t = 0)]
    pub limit: u32,
    /// Opaque cursor from a previous `next_cursor`.
    #[arg(long)]
    pub cursor: Option<String>,
    /// `asc` | `desc` (default `desc`).
    #[arg(long, default_value = "desc")]
    pub direction: String,
}

// --- dispatch ------------------------------------------------------

pub async fn run(client: &Client, cmd: Command) -> Result<Value> {
    match cmd {
        Command::Search(a) => search(client, a).await,
        Command::Resolve { wikilink } => resolve(client, wikilink).await,
        Command::Skill(SkillCmd::List) => list_skills(client).await,
        Command::Instance(InstanceCmd::List(a)) => list_instances(client, a).await,
        Command::Page(PageCmd::Expand { page_id }) => expand(client, page_id).await,
        Command::Page(PageCmd::Validate { page_id }) => validate(client, page_id).await,
        Command::Page(PageCmd::Update { page_id }) => update_page(client, page_id).await,
        Command::Link(LinkCmd::Neighbours(a)) => neighbours(client, a).await,
        Command::Event(c) => event_cmd(client, c).await,
        Command::Query(QueryCmd::Run(a)) => run_query(client, a).await,
        Command::Chat(ChatCmd::Append(a)) => chat_append(client, a).await,
        Command::Chat(ChatCmd::List(a)) => chat_list(client, a).await,
        // Admin / Ui are dispatched in main before reaching here.
        Command::Admin(_) => unreachable!("admin handled by admin::run"),
        Command::Ui => unreachable!("ui handled by escurel_tui::run"),
    }
}

fn read_stdin(what: &str) -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .with_context(|| format!("read {what} from stdin"))?;
    if buf.is_empty() {
        bail!("{what} is empty — pipe it into stdin");
    }
    Ok(buf)
}

async fn search(client: &Client, a: SearchArgs) -> Result<Value> {
    let resp = client
        .search(SearchRequest {
            q: a.q,
            k: a.k,
            granularity: String::new(),
            page_type: a.page_type,
            skill: a.skill.unwrap_or_default(),
            filter_json: String::new(),
            ..Default::default()
        })
        .await?;
    let hits: Vec<Value> = resp
        .hits
        .into_iter()
        .map(|h| {
            json!({
                "page_id": h.page_id,
                "slug": opt(&h.slug),
                "skill": h.skill,
                "page_type": h.page_type,
                "anchor": opt(&h.anchor),
                "snippet": h.snippet,
                "score": h.score,
                "frontmatter_excerpt": json_or_null(&h.frontmatter_excerpt_json),
            })
        })
        .collect();
    Ok(json!({ "hits": hits, "granularity": resp.granularity }))
}

async fn resolve(client: &Client, wikilink: String) -> Result<Value> {
    let resp = client
        .resolve(ResolveRequest {
            wikilink,
            ..Default::default()
        })
        .await?;
    Ok(json!({
        "exists": resp.exists,
        "parsed": resp.parsed.map(|p| json!({
            "skill": opt(&p.skill),
            "id": opt(&p.id),
            "anchor": opt(&p.anchor),
            "version": opt(&p.version),
            "alias": opt(&p.alias),
        })),
        "page": resp.page.map(page_ref),
    }))
}

async fn list_skills(client: &Client) -> Result<Value> {
    let resp = client.list_skills(ListSkillsRequest::default()).await?;
    let skills: Vec<Value> = resp
        .skills
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "description": s.description,
                "required_frontmatter": s.required_frontmatter,
                "optional_frontmatter": s.optional_frontmatter,
                "is_event_typed": s.is_event_typed,
            })
        })
        .collect();
    Ok(json!({ "skills": skills }))
}

async fn list_instances(client: &Client, a: InstanceListArgs) -> Result<Value> {
    let resp = client
        .list_instances(ListInstancesRequest {
            skill: a.skill,
            order_by_at: a.order_by_at,
            limit: a.limit,
            ..Default::default()
        })
        .await?;
    let instances: Vec<Value> = resp
        .instances
        .into_iter()
        .map(|i| {
            json!({
                "page_id": i.page_id,
                "skill": i.skill,
                "frontmatter": json_or_null(&i.frontmatter_json),
                "at": opt(&i.at),
            })
        })
        .collect();
    Ok(json!({ "instances": instances }))
}

async fn expand(client: &Client, page_id: String) -> Result<Value> {
    let resp = client
        .expand(ExpandRequest {
            page_id,
            anchor: String::new(),
            version: String::new(),
            ..Default::default()
        })
        .await?;
    Ok(json!({
        "page": resp.page.map(page_ref),
        "frontmatter": json_or_null(&resp.frontmatter_json),
        "body": resp.body,
        "blocks": resp.blocks.into_iter().map(|b| json!({
            "anchor": b.anchor,
            "content": b.content,
        })).collect::<Vec<_>>(),
        "wikilinks_out": resp.wikilinks_out.into_iter().map(|w| json!({
            "skill": opt(&w.skill),
            "id": opt(&w.id),
            "anchor": opt(&w.anchor),
            "version": opt(&w.version),
            "alias": opt(&w.alias),
        })).collect::<Vec<_>>(),
        "snapshot_version": opt(&resp.snapshot_version),
    }))
}

async fn validate(client: &Client, page_id: String) -> Result<Value> {
    let content = read_stdin("page body")?;
    let resp = client
        .validate(ValidateRequest { page_id, content })
        .await?;
    Ok(json!({
        "ok": resp.ok,
        "issues": resp.issues.into_iter().map(|i| json!({
            "code": i.code,
            "message": i.message,
            "anchor": opt(&i.anchor),
        })).collect::<Vec<_>>(),
    }))
}

async fn update_page(client: &Client, page_id: String) -> Result<Value> {
    let content = read_stdin("page body")?;
    let resp = client
        .update_page(UpdatePageRequest { page_id, content })
        .await?;
    Ok(json!({
        "ok": resp.ok,
        "issues": resp.issues.into_iter().map(|i| json!({
            "code": i.code,
            "message": i.message,
            "anchor": opt(&i.anchor),
        })).collect::<Vec<_>>(),
        "new_version": opt(&resp.new_version),
    }))
}

async fn neighbours(client: &Client, a: NeighboursArgs) -> Result<Value> {
    let resp = client
        .neighbours(NeighboursRequest {
            page_id: a.page_id,
            direction: a.direction,
            link_skill: a.link_skill.unwrap_or_default(),
            link_skill_in: Vec::new(),
            order_by: String::new(),
            limit: a.limit,
            ..Default::default()
        })
        .await?;
    let edges: Vec<Value> = resp
        .edges
        .into_iter()
        .map(|e| {
            json!({
                "src_page": e.src_page,
                "dst_page": e.dst_page,
                "link_skill": e.link_skill,
                "link_version": opt(&e.link_version),
                "dst_anchor": opt(&e.dst_anchor),
            })
        })
        .collect();
    Ok(json!({ "edges": edges }))
}

async fn event_cmd(client: &Client, cmd: EventCmd) -> Result<Value> {
    match cmd {
        EventCmd::Capture(a) => {
            let body = match a.body {
                Some(b) => b,
                None => read_stdin("event body")?,
            };
            let stored = client
                .capture_event(CaptureEventRequest {
                    event_id: a.event_id.unwrap_or_default(),
                    at: a.at.unwrap_or_default(),
                    source: a.source,
                    mime: a.mime,
                    label_skill: a.label_skill,
                    instance_page_id: a.instance.unwrap_or_default(),
                    title: a.title,
                    body,
                    provenance_json: a.provenance.unwrap_or_default(),
                })
                .await?;
            Ok(event(stored))
        }
        EventCmd::Inbox { limit } => {
            let resp = client.list_inbox(ListInboxRequest { limit }).await?;
            Ok(json!({ "events": resp.events.into_iter().map(event).collect::<Vec<_>>() }))
        }
        EventCmd::List { instance, limit } => {
            let resp = client
                .list_events(ListEventsRequest {
                    instance_page_id: instance,
                    limit,
                })
                .await?;
            Ok(json!({ "events": resp.events.into_iter().map(event).collect::<Vec<_>>() }))
        }
        EventCmd::Assign { event, instance } => {
            let ack = client
                .assign_event(AssignEventRequest {
                    event_id: event,
                    instance_page_id: instance,
                })
                .await?;
            Ok(json!({
                "event_id": ack.event_id,
                "instance_page_id": ack.instance_page_id,
            }))
        }
    }
}

async fn run_query(client: &Client, a: QueryRunArgs) -> Result<Value> {
    let resp = client
        .run_stored_query(RunStoredQueryRequest {
            query_id: a.query_id,
            params_json: a.params,
        })
        .await?;
    Ok(json!({
        "rows": json_or_null(&resp.rows_json),
        "schema": resp.schema.into_iter().map(|c| json!({
            "name": c.name,
            "type": c.type_name,
        })).collect::<Vec<_>>(),
    }))
}

async fn chat_append(client: &Client, a: ChatAppendArgs) -> Result<Value> {
    let content = match a.content {
        Some(c) => c,
        None => read_stdin("message content")?,
    };
    let resp = client
        .append_message(AppendMessageRequest {
            chat_group_id: a.group,
            role: a.role,
            content,
            author: a.author.unwrap_or_default(),
            ts: a.ts.unwrap_or_default(),
            metadata_json: a.metadata.unwrap_or_default(),
            msg_id: a.msg_id.unwrap_or_default(),
            embed: !a.no_embed,
        })
        .await?;
    Ok(json!({ "msg_id": resp.msg_id, "ts": resp.ts }))
}

async fn chat_list(client: &Client, a: ChatListArgs) -> Result<Value> {
    let resp = client
        .list_messages(ListMessagesRequest {
            chat_group_id: a.group,
            since: a.since.unwrap_or_default(),
            until: a.until.unwrap_or_default(),
            limit: a.limit,
            cursor: a.cursor.unwrap_or_default(),
            direction: a.direction,
        })
        .await?;
    Ok(json!({
        "messages": resp.messages.into_iter().map(|m| json!({
            "chat_group_id": m.chat_group_id,
            "msg_id": m.msg_id,
            "ts": m.ts,
            "role": m.role,
            "author": opt(&m.author),
            "content": m.content,
            "metadata": json_or_null(&m.metadata_json),
            "embedded": m.embedded,
        })).collect::<Vec<_>>(),
        "next_cursor": opt(&resp.next_cursor),
    }))
}
