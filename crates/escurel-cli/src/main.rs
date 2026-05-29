//! `escurel` — operator + agent-style CLI for the Escurel gateway.
//!
//! Thin gRPC client. All subcommands talk to the `escurel.v1.Escurel`
//! service over the gRPC endpoint configured via `--server` /
//! `ESCUREL_SERVER`. Auth via `--token` / `ESCUREL_TOKEN`.
//!
//! Today the agent surface is wired (8 RPCs). The admin surface
//! (`escurel admin tenant …`, `escurel admin rebuild …`) lands
//! alongside `EscurelAdmin` in M3.5d / M4.

use std::io::Read;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{
    AppendMessageRequest, ExpandRequest, ListInstancesRequest, ListMessagesRequest,
    ListSkillsRequest, NeighboursRequest, ResolveRequest, RunStoredQueryRequest, SearchRequest,
    UpdatePageRequest,
};
use serde_json::{Value, json};
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

#[derive(Parser, Debug)]
#[command(name = "escurel", about = "CLI for the Escurel gateway", version)]
struct Cli {
    /// gRPC endpoint URL, e.g. `http://127.0.0.1:8081`.
    #[arg(long, env = "ESCUREL_SERVER", default_value = "http://127.0.0.1:8081")]
    server: String,
    /// OIDC bearer token. Required unless the server runs
    /// unauthenticated (dev only).
    #[arg(long, env = "ESCUREL_TOKEN", hide_env_values = true)]
    token: Option<String>,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Return the tenant's Tier-1 skill catalogue.
    ListSkills,
    /// Enumerate instances of a skill.
    ListInstances(ListInstancesArgs),
    /// Parse a `[[wikilink]]` and look up its target page.
    Resolve { wikilink: String },
    /// Fetch a page's frontmatter + body + outbound wikilinks.
    Expand { page_id: String },
    /// Typed link-graph traversal.
    Neighbours(NeighboursArgs),
    /// Hybrid vector + FTS search.
    Search(SearchArgs),
    /// Execute a `[[query::<id>]]` instance with named parameters.
    RunStoredQuery(RunStoredQueryArgs),
    /// Upsert a markdown page. Body is read from stdin.
    UpdatePage { page_id: String },
    /// Per-chat-group conversation log (M-Chat, issue #63).
    #[command(subcommand)]
    Chat(ChatCommand),
}

#[derive(Subcommand, Debug)]
enum ChatCommand {
    /// Append a message to a chat group. Content is read from
    /// stdin unless `--content` is provided.
    Append(ChatAppendArgs),
    /// Read back a chat group's history.
    List(ChatListArgs),
}

#[derive(Args, Debug)]
struct ChatAppendArgs {
    /// Opaque chat-group id (consumer-defined).
    #[arg(long, short = 'g')]
    group: String,
    /// `user` | `assistant` | `system` | `tool`.
    #[arg(long, default_value = "user")]
    role: String,
    /// Message content. If absent, read from stdin.
    #[arg(long)]
    content: Option<String>,
    /// Opaque author handle.
    #[arg(long)]
    author: Option<String>,
    /// Event time (RFC-3339 UTC). Server stamps `CURRENT_TIMESTAMP`
    /// when absent.
    #[arg(long)]
    ts: Option<String>,
    /// Inline JSON metadata, e.g. `'{"thread":"t-42"}'`.
    #[arg(long)]
    metadata: Option<String>,
    /// Caller-supplied message id. Server generates a ULID when
    /// absent.
    #[arg(long)]
    msg_id: Option<String>,
    /// Skip embedding (cheap insert for high-volume sources).
    #[arg(long)]
    no_embed: bool,
}

#[derive(Args, Debug)]
struct ChatListArgs {
    #[arg(long, short = 'g')]
    group: String,
    /// Inclusive lower bound (RFC-3339).
    #[arg(long)]
    since: Option<String>,
    /// Exclusive upper bound (RFC-3339).
    #[arg(long)]
    until: Option<String>,
    /// 0 → server default (100); hard cap 1000.
    #[arg(long, default_value_t = 0)]
    limit: u32,
    /// Opaque cursor from a previous `next_cursor`.
    #[arg(long)]
    cursor: Option<String>,
    /// `asc` | `desc` (default `desc`).
    #[arg(long, default_value = "desc")]
    direction: String,
}

#[derive(Args, Debug)]
struct ListInstancesArgs {
    #[arg(long)]
    skill: String,
    /// "asc" | "desc"; empty for natural order.
    #[arg(long, default_value = "")]
    order_by_at: String,
    /// 0 means no limit.
    #[arg(long, default_value_t = 0)]
    limit: u32,
}

#[derive(Args, Debug)]
struct NeighboursArgs {
    page_id: String,
    /// "in" | "out" | "both" (default).
    #[arg(long, default_value = "both")]
    direction: String,
    /// Filter to a specific link skill (e.g. "meeting").
    #[arg(long)]
    link_skill: Option<String>,
    #[arg(long, default_value_t = 0)]
    limit: u32,
}

#[derive(Args, Debug)]
struct SearchArgs {
    /// Free-text query.
    q: String,
    /// Top-k hits. 0 → server default of 10.
    #[arg(long, default_value_t = 10)]
    k: u32,
    /// "skill" | "instance" | "any" (default).
    #[arg(long, default_value = "any")]
    page_type: String,
    /// Restrict to one skill.
    #[arg(long)]
    skill: Option<String>,
}

#[derive(Args, Debug)]
struct RunStoredQueryArgs {
    query_id: String,
    /// JSON object of parameters, e.g. `{"skill":"customer"}`.
    /// Defaults to `{}`.
    #[arg(long, default_value = "{}")]
    params: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // The verifier on the server is optional (dev / on-host mode
    // runs unauthenticated), so the CLI mirrors that: when no
    // token is configured, send the RPC without an `authorization`
    // metadata header and let the server enforce its own policy.
    let bearer: Option<MetadataValue<tonic::metadata::Ascii>> = match cli.token.as_deref() {
        Some(t) if !t.is_empty() => Some(
            format!("Bearer {t}")
                .parse()
                .context("token contains characters invalid in an HTTP header")?,
        ),
        _ => None,
    };

    let channel = Channel::from_shared(cli.server.clone())
        .with_context(|| format!("invalid --server URL `{}`", cli.server))?
        .connect()
        .await
        .with_context(|| format!("failed to connect to {}", cli.server))?;
    let mut client = EscurelClient::new(channel);

    let result: Value = match cli.cmd {
        Command::ListSkills => {
            let resp = client
                .list_skills(authed(ListSkillsRequest::default(), &bearer))
                .await?
                .into_inner();
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
            json!({ "skills": skills })
        }

        Command::ListInstances(a) => {
            let resp = client
                .list_instances(authed(
                    ListInstancesRequest {
                        skill: a.skill,
                        order_by_at: a.order_by_at,
                        limit: a.limit,
                        ..Default::default()
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            let instances: Vec<Value> = resp
                .instances
                .into_iter()
                .map(|i| {
                    json!({
                        "page_id": i.page_id,
                        "skill": i.skill,
                        "frontmatter": json_or_null(&i.frontmatter_json),
                        "at": optional_string(&i.at),
                    })
                })
                .collect();
            json!({ "instances": instances })
        }

        Command::Resolve { wikilink } => {
            let resp = client
                .resolve(authed(
                    ResolveRequest {
                        wikilink,
                        ..Default::default()
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            json!({
                "exists": resp.exists,
                "parsed": resp.parsed.map(|p| json!({
                    "skill": optional_string(&p.skill),
                    "id": optional_string(&p.id),
                    "anchor": optional_string(&p.anchor),
                    "version": optional_string(&p.version),
                    "alias": optional_string(&p.alias),
                })),
                "page": resp.page.map(page_ref_to_json),
            })
        }

        Command::Expand { page_id } => {
            let resp = client
                .expand(authed(
                    ExpandRequest {
                        page_id,
                        anchor: String::new(),
                        version: String::new(),
                        ..Default::default()
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            json!({
                "page": resp.page.map(page_ref_to_json),
                "frontmatter": json_or_null(&resp.frontmatter_json),
                "body": resp.body,
                "blocks": resp.blocks.into_iter().map(|b| json!({
                    "anchor": b.anchor,
                    "content": b.content,
                })).collect::<Vec<_>>(),
                "wikilinks_out": resp.wikilinks_out.into_iter().map(|w| json!({
                    "skill": optional_string(&w.skill),
                    "id": optional_string(&w.id),
                    "anchor": optional_string(&w.anchor),
                    "version": optional_string(&w.version),
                    "alias": optional_string(&w.alias),
                })).collect::<Vec<_>>(),
                "snapshot_version": optional_string(&resp.snapshot_version),
            })
        }

        Command::Neighbours(a) => {
            let resp = client
                .neighbours(authed(
                    NeighboursRequest {
                        page_id: a.page_id,
                        direction: a.direction,
                        link_skill: a.link_skill.unwrap_or_default(),
                        link_skill_in: Vec::new(),
                        order_by: String::new(),
                        limit: a.limit,
                        ..Default::default()
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            let edges: Vec<Value> = resp
                .edges
                .into_iter()
                .map(|e| {
                    json!({
                        "src_page": e.src_page,
                        "dst_page": e.dst_page,
                        "link_skill": e.link_skill,
                        "link_version": optional_string(&e.link_version),
                        "dst_anchor": optional_string(&e.dst_anchor),
                    })
                })
                .collect();
            json!({ "edges": edges })
        }

        Command::Search(a) => {
            let resp = client
                .search(authed(
                    SearchRequest {
                        q: a.q,
                        k: a.k,
                        granularity: String::new(),
                        page_type: a.page_type,
                        skill: a.skill.unwrap_or_default(),
                        filter_json: String::new(),
                        ..Default::default()
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            let hits: Vec<Value> = resp
                .hits
                .into_iter()
                .map(|h| {
                    json!({
                        "page_id": h.page_id,
                        "slug": optional_string(&h.slug),
                        "skill": h.skill,
                        "page_type": h.page_type,
                        "anchor": optional_string(&h.anchor),
                        "snippet": h.snippet,
                        "score": h.score,
                        "frontmatter_excerpt": json_or_null(&h.frontmatter_excerpt_json),
                    })
                })
                .collect();
            json!({
                "hits": hits,
                "granularity": resp.granularity,
            })
        }

        Command::RunStoredQuery(a) => {
            let resp = client
                .run_stored_query(authed(
                    RunStoredQueryRequest {
                        query_id: a.query_id,
                        params_json: a.params,
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            json!({
                "rows": json_or_null(&resp.rows_json),
                "schema": resp.schema.into_iter().map(|c| json!({
                    "name": c.name,
                    "type": c.type_name,
                })).collect::<Vec<_>>(),
            })
        }

        Command::UpdatePage { page_id } => {
            let mut content = String::new();
            std::io::stdin()
                .read_to_string(&mut content)
                .context("read page body from stdin")?;
            if content.is_empty() {
                bail!("page body is empty — pipe markdown into stdin");
            }
            let resp = client
                .update_page(authed(UpdatePageRequest { page_id, content }, &bearer))
                .await?
                .into_inner();
            json!({
                "ok": resp.ok,
                "issues": resp.issues.into_iter().map(|i| json!({
                    "code": i.code,
                    "message": i.message,
                    "anchor": optional_string(&i.anchor),
                })).collect::<Vec<_>>(),
                "new_version": optional_string(&resp.new_version),
            })
        }

        Command::Chat(ChatCommand::Append(a)) => {
            let content = match a.content {
                Some(c) => c,
                None => {
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buf)
                        .context("read message content from stdin")?;
                    if buf.is_empty() {
                        bail!("--content empty and stdin is empty");
                    }
                    buf
                }
            };
            let resp = client
                .append_message(authed(
                    AppendMessageRequest {
                        chat_group_id: a.group,
                        role: a.role,
                        content,
                        author: a.author.unwrap_or_default(),
                        ts: a.ts.unwrap_or_default(),
                        metadata_json: a.metadata.unwrap_or_default(),
                        msg_id: a.msg_id.unwrap_or_default(),
                        embed: !a.no_embed,
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            json!({ "msg_id": resp.msg_id, "ts": resp.ts })
        }

        Command::Chat(ChatCommand::List(a)) => {
            let resp = client
                .list_messages(authed(
                    ListMessagesRequest {
                        chat_group_id: a.group,
                        since: a.since.unwrap_or_default(),
                        until: a.until.unwrap_or_default(),
                        limit: a.limit,
                        cursor: a.cursor.unwrap_or_default(),
                        direction: a.direction,
                    },
                    &bearer,
                ))
                .await?
                .into_inner();
            json!({
                "messages": resp.messages.into_iter().map(|m| json!({
                    "chat_group_id": m.chat_group_id,
                    "msg_id": m.msg_id,
                    "ts": m.ts,
                    "role": m.role,
                    "author": optional_string(&m.author),
                    "content": m.content,
                    "metadata": json_or_null(&m.metadata_json),
                    "embedded": m.embedded,
                })).collect::<Vec<_>>(),
                "next_cursor": optional_string(&resp.next_cursor),
            })
        }
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn authed<T>(body: T, bearer: &Option<MetadataValue<tonic::metadata::Ascii>>) -> tonic::Request<T> {
    let mut req = tonic::Request::new(body);
    if let Some(b) = bearer {
        req.metadata_mut().insert("authorization", b.clone());
    }
    req
}

fn page_ref_to_json(p: escurel_proto::v1::PageRef) -> Value {
    json!({
        "page_id": p.page_id,
        "slug": optional_string(&p.slug),
        "skill": p.skill,
        "page_type": p.page_type,
    })
}

fn json_or_null(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_owned()))
    }
}

fn optional_string(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        Value::String(s.to_owned())
    }
}
