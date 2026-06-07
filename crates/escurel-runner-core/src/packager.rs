//! The context packager — "skills as instructions + tools" (#150).
//!
//! Lifecycle step 5 of
//! [`docs/contract/agent-orchestration.md`](https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md)
//! turns a [`Trigger`] into a [`TaskContext`]:
//!
//! - **Instructions** = the `label_skill` page body, fetched via
//!   `resolve("[[<label_skill>]]")` → `expand` over the client's `/mcp`
//!   surface. The packager prepends a short task framing and appends the
//!   triggering event's payload (title + body).
//! - **Input** = the event payload + the target instance's current state
//!   (`expand(instance_page_id)`) + its `list_events` history. When the
//!   trigger has no instance yet, the input notes the agent must create one.
//! - **Toolset pointer** = the gateway `/mcp` endpoint (config
//!   `gateway_url` → `<base>/mcp`) + a tenant-scoped bearer token, plus the
//!   narrowed [`allowed_tools`](TaskContext::allowed_tools) surface.
//!
//! Per the contract the packager only **reads** through the client
//! (`resolve`/`expand`/`list_events`); writes flow later through the
//! harness's own `/mcp` tool calls.

use escurel_client::{Client, ExpandRequest, ListEventsRequest, ListInboxRequest, ResolveRequest};
use escurel_types::Event;
use secrecy::{ExposeSecret, SecretString};

use crate::{RunnerConfig, Trigger};

/// The narrowed agent tool surface a packaged run is allowed to call —
/// the read tools plus the write-capable subset
/// (`validate`/`update_page`/`assign_event`/`capture_event`) named by the
/// contract's "Skills as instructions + tools" section. This is the
/// `allowedTools` list handed to the harness's MCP config.
pub const ALLOWED_TOOLS: &[&str] = &[
    // read surface
    "list_skills",
    "list_instances",
    "resolve",
    "expand",
    "neighbours",
    "search",
    "run_stored_query",
    "list_events",
    "list_inbox",
    "list_messages",
    // write-capable subset
    "validate",
    "update_page",
    "append_message",
    "capture_event",
    "assign_event",
];

/// How many of an instance's recent events to fold into the input. The
/// agent gets enough history to act without drowning in it; the instance
/// page itself is the authoritative current state.
const EVENT_HISTORY_LIMIT: u32 = 20;

/// Errors raised while packaging a [`Trigger`] into a [`TaskContext`].
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// A read call against the gateway's `/mcp` surface failed.
    #[error("gateway call {call} failed: {source}")]
    Client {
        /// The logical step that failed (`resolve` / `expand` /
        /// `list_events`).
        call: &'static str,
        /// The underlying transport/protocol error.
        #[source]
        source: escurel_client::Error,
    },
    /// `resolve("[[<label_skill>]]")` did not resolve to a page — the
    /// skill named by the trigger does not exist in the tenant store, so
    /// there are no instructions to package.
    #[error("label_skill {skill:?} did not resolve to a skill page")]
    SkillNotFound {
        /// The unresolved skill name.
        skill: String,
    },
    /// The runner is not configured with a tenant-scoped token, so the
    /// packaged toolset pointer would carry no usable bearer.
    #[error("no ESCUREL_RUNNER_TOKEN configured; cannot mint a scoped toolset token")]
    MissingToken,
}

/// The packaged unit of work handed to a harness adapter: the skill body as
/// instructions, the event + instance state as input, and a pointer at the
/// gateway `/mcp` toolset with a scoped bearer.
///
/// The bearer is held opaque in a [`SecretString`] and a manual [`Debug`]
/// impl redacts it, so logging a `TaskContext` never leaks the token.
#[derive(Clone)]
pub struct TaskContext {
    /// The agent's instructions: the task framing + the resolved skill
    /// body + the triggering event payload.
    pub instructions: String,
    /// The agent's input: the event payload + the target instance's
    /// current state + its event history (or a "create a new instance"
    /// note when the trigger has no instance yet).
    pub input: String,
    /// The gateway `/mcp` endpoint the harness declares as its MCP server.
    pub mcp_endpoint: String,
    /// The narrowed tool surface the run may call (see [`ALLOWED_TOOLS`]).
    pub allowed_tools: Vec<String>,
    /// Tenant-scoped bearer for the `/mcp` toolset, held opaque.
    ///
    /// For now this reuses the configured `ESCUREL_RUNNER_TOKEN`. The
    /// per-run short-TTL minted `Role::Agent` JWT (the contract's "freshly
    /// minted, short-TTL" token) is a later concern — this field is the
    /// seam where that minting will land without changing the public shape.
    token: SecretString,
}

impl TaskContext {
    /// Expose the scoped bearer as a `&str`. The harness adapter (#155+)
    /// wires this into the `/mcp` auth header; the DoD integration test
    /// uses it to prove the packaged token is a usable agent bearer.
    ///
    /// The token stays out of [`Debug`]/[`Clone`]-derived logging via the
    /// [`SecretString`] field + the manual `Debug` impl below; this is the
    /// single, explicit read path.
    pub fn token_str(&self) -> &str {
        self.token.expose_secret()
    }

    /// Construct a `TaskContext` directly from its parts.
    ///
    /// The normal construction path is [`package`], which reads the skill +
    /// instance state off the gateway. This constructor exists so harness
    /// adapters (and their tests) can build a `TaskContext` by hand to
    /// exercise the invocation-build / outcome-parse path without standing up
    /// a gateway. The bearer is wrapped opaquely exactly as [`package`] does.
    pub fn for_test(
        instructions: String,
        input: String,
        mcp_endpoint: String,
        allowed_tools: Vec<String>,
        token: SecretString,
    ) -> Self {
        Self {
            instructions,
            input,
            mcp_endpoint,
            allowed_tools,
            token,
        }
    }
}

impl std::fmt::Debug for TaskContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskContext")
            .field("instructions", &self.instructions)
            .field("input", &self.input)
            .field("mcp_endpoint", &self.mcp_endpoint)
            .field("allowed_tools", &self.allowed_tools)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Compute the gateway `/mcp` endpoint from the configured base URL,
/// tolerating a trailing slash so `http://gw:8080` and `http://gw:8080/`
/// both yield `http://gw:8080/mcp`.
fn mcp_endpoint(gateway_url: &str) -> String {
    format!("{}/mcp", gateway_url.trim_end_matches('/'))
}

/// Package a [`Trigger`] into a [`TaskContext`].
///
/// Reads (only) through `client`: `resolve("[[<label_skill>]]")` →
/// `expand` for the instructions, and `expand` + `list_events` for the
/// instance input. Never writes — writes are the harness's job over `/mcp`.
pub async fn package(
    trigger: &Trigger,
    client: &Client,
    cfg: &RunnerConfig,
) -> Result<TaskContext, PackageError> {
    let token = cfg
        .token
        .clone()
        .ok_or(PackageError::MissingToken)
        .map(SecretString::from)?;

    // ── Instructions: resolve the skill wikilink → expand its body. ──
    let resolved = client
        .resolve(ResolveRequest {
            wikilink: format!("[[{}]]", trigger.label_skill),
            ..Default::default()
        })
        .await
        .map_err(|source| PackageError::Client {
            call: "resolve",
            source,
        })?;
    let skill_page = resolved.page.ok_or_else(|| PackageError::SkillNotFound {
        skill: trigger.label_skill.clone(),
    })?;
    let skill = client
        .expand(ExpandRequest {
            page_id: skill_page.page_id,
            ..Default::default()
        })
        .await
        .map_err(|source| PackageError::Client {
            call: "expand",
            source,
        })?;

    // ── Input + the triggering event payload. ──
    //
    // The instance's own page is the authoritative current state; its
    // `list_events` history carries the full event records (title + body),
    // including the one that triggered this run (assigned just before
    // dispatch). For an unassigned trigger the event is still in the inbox,
    // so we read it from `list_inbox` instead. Either way we recover the
    // triggering event's payload so the instructions can append it.
    let (input, trigger_event) = match &trigger.instance_page_id {
        Some(instance_page_id) => {
            let instance = client
                .expand(ExpandRequest {
                    page_id: instance_page_id.clone(),
                    ..Default::default()
                })
                .await
                .map_err(|source| PackageError::Client {
                    call: "expand",
                    source,
                })?;
            let history = client
                .list_events(ListEventsRequest {
                    instance_page_id: instance_page_id.clone(),
                    limit: EVENT_HISTORY_LIMIT,
                })
                .await
                .map_err(|source| PackageError::Client {
                    call: "list_events",
                    source,
                })?;
            let trigger_event = history
                .events
                .iter()
                .find(|e| e.event_id == trigger.event_id)
                .cloned();
            let input = build_input_for_instance(
                trigger,
                instance_page_id,
                &instance.body,
                &history.events,
            );
            (input, trigger_event)
        }
        None => {
            let inbox = client
                .list_inbox(ListInboxRequest {
                    limit: EVENT_HISTORY_LIMIT,
                })
                .await
                .map_err(|source| PackageError::Client {
                    call: "list_inbox",
                    source,
                })?;
            let trigger_event = inbox
                .events
                .iter()
                .find(|e| e.event_id == trigger.event_id)
                .cloned();
            (build_input_for_new_instance(trigger), trigger_event)
        }
    };

    let instructions = build_instructions(trigger, &skill.body, trigger_event.as_ref());

    Ok(TaskContext {
        instructions,
        input,
        mcp_endpoint: mcp_endpoint(&cfg.gateway_url),
        allowed_tools: ALLOWED_TOOLS.iter().map(|s| s.to_string()).collect(),
        token,
    })
}

/// Render the triggering event's payload (title + body when known). When
/// the event record could not be recovered we fall back to the ids the
/// trigger carries so the framing is still coherent.
fn render_event_payload(trigger: &Trigger, event: Option<&Event>) -> String {
    match event {
        Some(e) => format!(
            "event_id: {}\nlabel_skill: {}\nsource: {}\ntitle: {}\n\n{}\n",
            e.event_id, e.label_skill, e.source, e.title, e.body
        ),
        None => format!(
            "event_id: {}\nlabel_skill: {}\n",
            trigger.event_id, trigger.label_skill
        ),
    }
}

/// Build the instructions: a short task framing, the skill body, and the
/// triggering event payload appended at the end.
fn build_instructions(trigger: &Trigger, skill_body: &str, event: Option<&Event>) -> String {
    format!(
        "A new event of type `{skill}` arrived (event `{event_id}`). Fold it into the \
         appropriate `{skill}` instance per the skill below.\n\n\
         ## Skill: {skill}\n\n{skill_body}\n\n\
         ## Triggering event\n\n{payload}",
        skill = trigger.label_skill,
        event_id = trigger.event_id,
        skill_body = skill_body.trim_end(),
        payload = render_event_payload(trigger, event),
    )
}

/// Build the input for a trigger that already targets an instance: the
/// event reference, the instance's current expanded state, and its event
/// history.
fn build_input_for_instance(
    trigger: &Trigger,
    instance_page_id: &str,
    instance_body: &str,
    history: &[Event],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## Triggering event\n\nevent_id: {}\nlabel_skill: {}\n\n",
        trigger.event_id, trigger.label_skill
    ));
    out.push_str(&format!(
        "## Target instance ({instance_page_id})\n\n{}\n\n",
        instance_body.trim_end()
    ));
    out.push_str(&format!(
        "## Instance event history ({} event(s))\n\n",
        history.len()
    ));
    if history.is_empty() {
        out.push_str("(no prior events)\n");
    } else {
        for e in history {
            out.push_str(&format!(
                "- {} [{}] {}: {}\n",
                e.event_id, e.label_skill, e.title, e.body
            ));
        }
    }
    out
}

/// Build the input for a trigger with no instance yet: note that the agent
/// must create one, and carry the event reference.
fn build_input_for_new_instance(trigger: &Trigger) -> String {
    format!(
        "## Triggering event\n\nevent_id: {}\nlabel_skill: {}\n\n\
         ## Target instance\n\n\
         No instance is assigned to this event yet. Per the skill, create a new \
         `{}` instance for it (and `assign_event` the event to the page you create).\n",
        trigger.event_id, trigger.label_skill, trigger.label_skill
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_endpoint_appends_mcp_and_tolerates_trailing_slash() {
        assert_eq!(mcp_endpoint("http://gw:8080"), "http://gw:8080/mcp");
        assert_eq!(mcp_endpoint("http://gw:8080/"), "http://gw:8080/mcp");
    }

    #[test]
    fn allowed_tools_include_the_write_capable_subset() {
        for t in ["update_page", "assign_event", "validate", "capture_event"] {
            assert!(ALLOWED_TOOLS.contains(&t), "missing {t}");
        }
    }

    #[test]
    fn instructions_carry_framing_skill_body_and_event() {
        let trigger = Trigger {
            tenant: "acme".into(),
            event_id: "EVT1".into(),
            label_skill: "note".into(),
            instance_page_id: None,
            lineage: crate::Lineage::root("EVT1"),
        };
        let event = Event {
            event_id: "EVT1".into(),
            label_skill: "note".into(),
            source: "manual".into(),
            title: "TITLEMARK".into(),
            body: "BODYMARK".into(),
            ..Event::default()
        };
        let instr = build_instructions(&trigger, "SKILLBODY", Some(&event));
        assert!(instr.contains("note"));
        assert!(instr.contains("SKILLBODY"));
        assert!(instr.contains("EVT1"));
        assert!(
            instr.contains("TITLEMARK"),
            "event title folded in: {instr}"
        );
        assert!(instr.contains("BODYMARK"), "event body folded in: {instr}");

        // No event record recovered → fall back to the trigger ids.
        let fallback = build_instructions(&trigger, "SKILLBODY", None);
        assert!(fallback.contains("EVT1"));
    }

    #[test]
    fn new_instance_input_tells_the_agent_to_create_one() {
        let trigger = Trigger {
            tenant: "acme".into(),
            event_id: "EVT1".into(),
            label_skill: "note".into(),
            instance_page_id: None,
            lineage: crate::Lineage::root("EVT1"),
        };
        let input = build_input_for_new_instance(&trigger);
        assert!(input.contains("create a new"));
        assert!(input.contains("EVT1"));
    }

    #[test]
    fn debug_redacts_the_token() {
        let ctx = TaskContext {
            instructions: "i".into(),
            input: "in".into(),
            mcp_endpoint: "http://gw/mcp".into(),
            allowed_tools: vec!["update_page".into()],
            token: SecretString::from("super-secret-token".to_string()),
        };
        let dbg = format!("{ctx:?}");
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("super-secret-token"));
        // token_str() still hands the real secret to explicit callers.
        assert_eq!(ctx.token_str(), "super-secret-token");
    }
}
