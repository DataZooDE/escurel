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

/// The narrowed tool surface for a run that must NOT commit: the read
/// surface + `validate` + `create_draft`, and nothing that lands bytes.
///
/// `update_page` is absent, so a review run cannot write the page even if
/// the model decides to. `assign_event` is absent too, and that is the part
/// worth stating: the event stays in the inbox until a human promotes the
/// draft, because marking it `processed` would say the knowledge base has
/// absorbed something it has not. `capture_event` is absent for the same
/// reason `WORKFLOW_STEP_TOOLS` denies it — a run that cannot land its own
/// write must not be able to fan out others.
pub const REVIEW_TOOLS: &[&str] = &[
    "list_skills",
    "list_instances",
    "resolve",
    "expand",
    "neighbours",
    "search",
    "list_events",
    "list_inbox",
    "list_messages",
    "validate",
    "create_draft",
    "append_message",
];

/// What a skill's `autonomy:` declaration means for a run.
///
/// escurel has published this per skill on `list_skills` since #360 and
/// enforced nothing — the doc comment says so outright: it "reports what the
/// page declares, so a client can render the gate". Every client rendered it
/// differently or not at all, and the runner committed regardless. This is
/// where the declaration becomes behaviour.
///
/// Two values, not three: `confirm` and `review` differ in what a HUMAN is
/// asked, not in what the runner may write, and both mean "do not land it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autonomy {
    /// The agent commits directly.
    Auto,
    /// The agent produces a draft; a human lands it.
    Review,
}

impl Autonomy {
    /// Read a skill page's `autonomy:` frontmatter.
    ///
    /// **Absent or unrecognised is `Review`.** Only the exact string `auto`
    /// buys unattended writes. A typo (`atuo:`) that silently meant "commit
    /// without a gate" is the one direction this must not fail in, and the
    /// cost of the opposite mistake is a human seeing a draft they would
    /// have approved anyway.
    #[must_use]
    pub fn from_frontmatter(frontmatter: &serde_json::Value) -> Self {
        match frontmatter.get("autonomy").and_then(|v| v.as_str()) {
            Some("auto") => Self::Auto,
            _ => Self::Review,
        }
    }

    /// The tool surface a run under this policy may call.
    #[must_use]
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Auto => ALLOWED_TOOLS,
            Self::Review => REVIEW_TOOLS,
        }
    }
}

/// The narrowed tool surface for a **workflow step** agent (`§7`, injection
/// containment). A step agent's job is to write its phase's `produces:`
/// instance and nothing else, so it gets the read surface + `validate` +
/// `update_page` + `append_message`, but is **denied `capture_event` and
/// `assign_event`** — the event surface. The runner (not the agent) owns
/// every decision to emit or assign an event, so a step reading an untrusted
/// fetched page cannot capture an event to steer the run's phase sequence.
/// (The reducer is instance-driven and ignores agent-captured events anyway;
/// this is defence in depth.)
pub const WORKFLOW_STEP_TOOLS: &[&str] = &[
    "list_skills",
    "list_instances",
    "resolve",
    "expand",
    "neighbours",
    "search",
    "list_events",
    "list_inbox",
    "list_messages",
    "validate",
    "update_page",
    "append_message",
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
    #[error(
        "no runner credential configured; set ESCUREL_RUNNER_TOKEN, or \
         ESCUREL_RUNNER_AUTH_ISSUER + ESCUREL_RUNNER_AUTH_SIGNING_KEY to mint one"
    )]
    MissingToken,
    /// A credential is configured but could not be minted.
    #[error("could not mint the runner's gateway bearer: {0}")]
    Auth(String),
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
    /// What the triggering skill's `autonomy:` declared. The reconciler
    /// reads it to know WHAT to confirm — a landed write, or a held one —
    /// and the dispatch loop reads it to know that a held write must not
    /// cascade.
    pub autonomy: Autonomy,
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
            // The gated policy, so a hand-built context in a test never
            // silently exercises the committing surface.
            autonomy: Autonomy::Review,
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
    tokens: Option<&crate::TokenSource>,
) -> Result<TaskContext, PackageError> {
    // Taken from the source at PACKAGE time, not held from boot: a minted
    // bearer is re-minted before it lapses, and a run packaged with an
    // expired one fails every `/mcp` call while the process looks healthy.
    let token = tokens
        .ok_or(PackageError::MissingToken)?
        .current()
        .map_err(|e| PackageError::Auth(e.to_string()))
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
                    ..Default::default()
                })
                .await
                .map_err(|source| PackageError::Client {
                    call: "list_events",
                    source,
                })?;
            let mut trigger_event = history
                .events
                .iter()
                .find(|e| e.event_id == trigger.event_id)
                .cloned();
            // A pre-flagged event is NOT in the instance's history: it is
            // still in the inbox until something assigns it, and
            // `list_events` returns processed history only. So the branch
            // that had a target found no event and fell back to rendering
            // two ids — the agent was asked to fold an event whose title,
            // body and provenance it had never been shown, and it could not
            // say so, because a plausible answer is always available.
            //
            // Invisible until now because the deployed loop never ran, and
            // because the tests that did run captured events the harness
            // then assigned itself before anyone looked.
            if trigger_event.is_none() {
                let inbox = client
                    .list_inbox(ListInboxRequest {
                        cursor: String::new(),
                        limit: EVENT_HISTORY_LIMIT,
                    })
                    .await
                    .map_err(|source| PackageError::Client {
                        call: "list_inbox",
                        source,
                    })?;
                trigger_event = inbox
                    .events
                    .iter()
                    .find(|e| e.event_id == trigger.event_id)
                    .cloned();
            }
            let input = build_input_for_instance(
                trigger,
                trigger_event.as_ref(),
                instance_page_id,
                &instance.body,
                &history.events,
            );
            (input, trigger_event)
        }
        None => {
            let inbox = client
                .list_inbox(ListInboxRequest {
                    cursor: String::new(),
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
            (
                build_input_for_new_instance(trigger, trigger_event.as_ref()),
                trigger_event,
            )
        }
    };

    // What the skill declared — except for a workflow step, which is
    // `Auto` by construction.
    //
    // That is not an exemption smuggled in: a workflow runs because a human
    // invoked a named plan, and the plan's phases say what will be written
    // before anything runs. The gate `autonomy:` exists for is the one
    // between an EVENT arriving and a page changing with nobody having asked
    // for it. A step also keeps `WORKFLOW_STEP_TOOLS`, which is narrower than
    // the committing surface in the direction that matters (no event tools).
    //
    // The two must agree: the reconciler confirms a landed write or a draft
    // depending on this value, so a step packaged with committing tools and
    // reconciled as a draft would dead-letter every workflow run.
    let autonomy = if trigger.workflow.is_some() {
        Autonomy::Auto
    } else {
        Autonomy::from_frontmatter(&skill.frontmatter)
    };
    let tools = if trigger.workflow.is_some() {
        WORKFLOW_STEP_TOOLS
    } else {
        autonomy.tools()
    };

    let instructions = build_instructions(trigger, &skill.body, trigger_event.as_ref(), autonomy);

    Ok(TaskContext {
        instructions,
        input,
        autonomy,
        mcp_endpoint: mcp_endpoint(&cfg.gateway_url),
        allowed_tools: tools.iter().map(|s| s.to_string()).collect(),
        token,
    })
}

/// Render the triggering event's payload (title + body when known). When
/// the event record could not be recovered we fall back to the ids the
/// trigger carries so the framing is still coherent.
fn render_event_payload(trigger: &Trigger, event: Option<&Event>) -> String {
    match event {
        Some(e) => format!(
            "event_id: {}\nlabel_skill: {}\nsource: {}\ntitle: {}\n{}\n{}\n",
            e.event_id,
            e.label_skill,
            e.source,
            e.title,
            render_provenance(&e.provenance),
            e.body
        ),
        None => format!(
            "event_id: {}\nlabel_skill: {}\n",
            trigger.event_id, trigger.label_skill
        ),
    }
}

/// The event's `provenance`, as a line the agent can read — or nothing.
///
/// This was missing, and it is the half of attribution the agent could not
/// see. A capture that arrived through an AUTHORED route carries the
/// engagement it was routed to (heron's P3.3, "authored, never inferred"),
/// and a chat capture carries the conversation it belongs to. Both live in
/// `provenance`, and none of it reached the model: the agent was asked which
/// customer an email concerns while being shown everything about it EXCEPT
/// the one field a human had already answered that question in.
///
/// Rendered as compact JSON rather than prose: it is a fact the event
/// carries, not a claim this packager makes, and the shape says so.
/// Server-stamped `captured_by` travels in the same object, so "who" and
/// "on whose behalf" stay together.
fn render_provenance(provenance: &serde_json::Value) -> String {
    if provenance.is_null() || provenance.as_object().is_some_and(|o| o.is_empty()) {
        return String::new();
    }
    format!("provenance: {provenance}")
}

/// Build the instructions: a short task framing plus the skill body.
///
/// Deliberately WITHOUT the event payload. The instructions become
/// `claude --append-system-prompt <string>` and codex's prompt framing —
/// both argv, and Linux caps a single argv string at 32 pages. A 220 KB
/// meeting transcript here made `spawn` fail with E2BIG and the event
/// permanently undispatchable.
///
/// It is the right split independently of that limit: the system prompt
/// carries the PROCEDURE, the input carries the DATA.
fn build_instructions(
    trigger: &Trigger,
    skill_body: &str,
    event: Option<&Event>,
    autonomy: Autonomy,
) -> String {
    let title = event.map(|e| e.title.as_str()).unwrap_or("");
    // Under review the tool surface already makes committing impossible, but
    // a model that discovers this by calling a tool it was never given burns
    // a turn and writes a confused transcript. Say it once, plainly.
    let gate = match autonomy {
        Autonomy::Auto => String::new(),
        Autonomy::Review => format!(
            "\n\n## This change must be REVIEWED before it lands\n\n\
             `{skill}` declares `autonomy: review`, so you do not write pages. \
             Follow the procedure above as written, and wherever it tells you to \
             WRITE a page, create a draft of that page instead:\n\n\
             - read the target with `expand`;\n\
             - compose the WHOLE markdown you would have written;\n\
             - call `create_draft` with `target_page_id`, that `content`, and \
             `base_sha256` set to the target's `content_sha256` from `expand` (an \
             empty string when no page exists yet).\n\n\
             **One draft per page.** If the procedure produces several pages — an \
             artifact and a typed fact promoted out of it, say — draft each of \
             them. Do not collapse them into one document.\n\n\
             `create_draft` VALIDATES, and on the draft path EVERY key the skill \
             declares in `required_frontmatter` is required — not just `id` and \
             `skill`. A refusal comes back as `{{ok:false, issues:[…]}}` naming \
             what is wrong; fix that and call it again. A refused draft is not a \
             failed run.\n\n\
             **If you cannot determine a required field, do not invent one and do \
             not draft.** `engagement` in particular says WHOSE this is, and it is \
             answered by a human or by an authored routing rule — never inferred \
             from the content. An event that arrives without one is a question for \
             a person, not a gap for you to fill: say so and stop. A draft nobody \
             can attribute is a draft nobody can be shown, and it is worse than no \
             draft at all.\n\n\
             Do not try to write or assign — you have neither verb. The event \
             stays in the inbox on purpose until a human decides.",
            skill = trigger.label_skill,
        ),
    };
    // **The trigger already chose the page. Say so in the INSTRUCTIONS.**
    //
    // "Fold it into the appropriate instance" invites the model to pick one,
    // and the target page id was only ever stated in the input, under a
    // heading among several. Measured 2026-09-09 against the shipped Gemini
    // harness: a workflow phase assigned to one page wrote
    // `deep-research/workflow-run/gem.md` on its first attempt and
    // `deep-research/<event_id>.md` on its second, self-reported `ok` both
    // times, and the workflow never converged — the next phase reads the page
    // the runner NAMED. This was invisible for as long as it was, because the
    // harness those tests drove was a script that took the page id from the
    // event and never asked the model.
    let target = match &trigger.instance_page_id {
        Some(page) => {
            // `markdown/instances/<skill>/<id>.md`. Derived from the PAGE, not
            // from `label_skill`: a workflow board is `workflow-run/<run>.md`
            // while the trigger's skill is the workflow's own — telling the
            // agent to stamp the trigger's skill there would author an invalid
            // page in the one place it matters most.
            // **The completion step, said out loud on the auto path.**
            //
            // The reconciler's definition of done is "the event is processed
            // ON that page" — nothing else ends the run, and an agent that
            // writes the page and stops leaves a run that retries, exhausts
            // its attempts and dead-letters work that actually landed. The
            // review gate has always stated its own protocol (draft, do not
            // assign); this is the same courtesy for the path that commits.
            // Measured 2026-09-09: with the page finally valid, every attempt
            // still failed `not yet processed`, because nobody had told the
            // agent that assigning is how a run finishes.
            let finish = match autonomy {
                Autonomy::Auto => format!(
                    "\n\nWhen the write has landed, call `assign_event` with this \
                     event's id and `{page}` — that is what marks the event \
                     processed and ends the run. A page written without it reads \
                     as unfinished work and will be re-run."
                ),
                Autonomy::Review => String::new(),
            };
            // The verb this run actually has. Naming `update_page` to a
            // review run is worse than useless: it has no such tool, and the
            // gate directly below tells it to draft instead — so the page
            // contract has to be stated in terms of the write it CAN make.
            let (verb, write) = match autonomy {
                Autonomy::Auto => ("`update_page`", "write"),
                Autonomy::Review => ("`create_draft`", "draft"),
            };
            let fields = match page
                .strip_prefix("markdown/instances/")
                .and_then(|rest| rest.strip_suffix(".md"))
                .and_then(|rest| rest.split_once('/'))
            {
                Some((skill, id)) => format!(
                    " `type: instance`, `skill: {skill}` and `id: {id}` (from that \
                     page id),"
                ),
                None => " `type: instance` and a `skill` and `id` matching that \
                          page id,"
                    .to_owned(),
            };
            format!(
                "\n\n## Write to this page, and only this page\n\n\
             This event is already assigned to `{page}`. That id IS the answer \
             to \"which instance?\" — use it exactly, as the `page_id` of your \
             write (or the `target_page_id` of your draft). Do not shorten it, \
             derive one from the event id, or invent a new one, even if the \
             skill's procedure describes how ids are normally chosen: that \
             choice was already made. A write to any other id is lost work — it \
             looks like success and nothing downstream reads it.\n\n\
             The page you {write} must be a VALID instance page or the store \
             refuses it: it starts with a `---` frontmatter block that is a \
             YAML mapping, and that block carries at least{fields} plus every \
             key the skill declares in `required_frontmatter`. {verb} \
             VALIDATES: a refusal comes back as `{{ok:false, issues:[…]}}` \
             naming exactly what is wrong. Fix that and call it again — a \
             rejected write left unfixed is not a page, however the run reads.\
             {finish}"
            )
        }
        None => String::new(),
    };
    // **A workflow step's coordinates, spelled out as frontmatter it must
    // stamp.**
    //
    // The reducer advances a run by READING the pages its steps produced:
    // `vote_from_instance` tallies `COUNT(DISTINCT vote_index)` off each
    // `verify-vote`'s frontmatter, and a vote missing `claim`/`vote_index` is
    // ignored rather than counted. `WorkflowProvenance::vote_index` says so in
    // its own doc comment — "the index a `verify-vote` harness MUST stamp into
    // its instance" — and nothing ever told the agent. The Python runner this
    // replaced stamped them itself, which is why the requirement could go
    // unstated for as long as it did: measured 2026-09-09, three skeptics
    // authored three real votes, none carried a slot, the barrier never closed
    // and the run stopped with no error anywhere.
    let coordinates = match (&trigger.workflow, autonomy) {
        (Some(wf), Autonomy::Auto) => {
            let mut lines = format!(
                "\n\n## You are one step of a workflow\n\n\
                 Phase `{phase}` of `{wf_skill}`, run board `{run}`. Carry \
                 `workflow_run: {run}` in the frontmatter of the page you write.",
                phase = wf.phase,
                wf_skill = wf.wf_skill,
                run = wf.run,
            );
            if let Some(index) = wf.vote_index {
                // The element this step votes on, as the tally keys it: the
                // last path segment of `over`, without the `.md`.
                let claim = wf
                    .over
                    .rsplit('/')
                    .next()
                    .unwrap_or(wf.over.as_str())
                    .trim_end_matches(".md");
                lines.push_str(&format!(
                    "\n\nYou are skeptic #{index} of several, each reviewing the \
                     same material independently. Your page MUST carry, in its \
                     frontmatter:\n\n\
                     - `vote_index: {index}` — your slot. It is not derivable \
                     from anything you can see, and two skeptics sharing a slot \
                     count as one vote.\n\
                     - `claim: {claim}` — what is under review.\n\
                     - `verdict: valid` | `refuted` | `unverified` — your \
                     judgement, and the only one of the three that is yours to \
                     decide.\n\n\
                     A vote missing any of them is not counted, the barrier \
                     never closes, and the whole run stops with no error \
                     reported anywhere."
                ));
            }
            lines
        }
        _ => String::new(),
    };
    format!(
        "A new event of type `{skill}` arrived (event `{event_id}`{title}). Fold it into \
         the appropriate `{skill}` instance per the skill below. The event itself is in \
         the task input.\n\n\
         ## Skill: {skill}\n\n{skill_body}{coordinates}{target}{gate}",
        skill = trigger.label_skill,
        event_id = trigger.event_id,
        title = if title.is_empty() {
            String::new()
        } else {
            format!(", {title:?}")
        },
        skill_body = skill_body.trim_end(),
    )
}

/// Build the input for a trigger that already targets an instance: the
/// event reference, the instance's current expanded state, and its event
/// history.
fn build_input_for_instance(
    trigger: &Trigger,
    event: Option<&Event>,
    instance_page_id: &str,
    instance_body: &str,
    history: &[Event],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## Triggering event\n\n{}\n",
        render_event_payload(trigger, event)
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
fn build_input_for_new_instance(trigger: &Trigger, event: Option<&Event>) -> String {
    format!(
        "## Triggering event\n\n{payload}\n\
         ## Target instance\n\n\
         No instance is assigned to this event yet. Per the skill, create a new \
         `{skill}` instance for it (and `assign_event` the event to the page you \
         create).\n",
        payload = render_event_payload(trigger, event),
        skill = trigger.label_skill,
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

    /// The agent must SEE what a human already decided about the event.
    #[test]
    fn the_event_payload_carries_provenance_when_there_is_any() {
        let trigger = Trigger {
            tenant: "acme".into(),
            event_id: "EVT1".into(),
            label_skill: "email".into(),
            instance_page_id: None,
            lineage: crate::Lineage::root("EVT1"),
            workflow: None,
            content_hash: None,
        };
        let routed = Event {
            event_id: "EVT1".into(),
            label_skill: "email".into(),
            title: "Renewal".into(),
            body: "BODYMARK".into(),
            provenance: serde_json::json!({
                "engagement": "engagement-hoffmann",
                "captured_by": "consultant:alice",
            }),
            ..Event::default()
        };
        let rendered = render_event_payload(&trigger, Some(&routed));
        assert!(
            rendered.contains("engagement-hoffmann"),
            "an authored route's engagement must reach the agent — it is the \
             question the agent is being asked, already answered: {rendered}"
        );
        assert!(rendered.contains("consultant:alice"), "{rendered}");
        assert!(rendered.contains("BODYMARK"), "{rendered}");

        // Positive control for the absence below: the SAME event without
        // provenance renders without an empty `provenance:` line, so a model
        // is never shown a field that says nothing.
        let bare = Event {
            provenance: serde_json::Value::Null,
            ..routed.clone()
        };
        let rendered = render_event_payload(&trigger, Some(&bare));
        assert!(!rendered.contains("provenance"), "{rendered}");
        assert!(rendered.contains("BODYMARK"), "{rendered}");
    }

    #[test]
    fn allowed_tools_include_the_write_capable_subset() {
        for t in ["update_page", "assign_event", "validate", "capture_event"] {
            assert!(ALLOWED_TOOLS.contains(&t), "missing {t}");
        }
    }

    #[test]
    fn workflow_step_tools_deny_the_event_surface_but_keep_writes() {
        // Injection containment (§7): a workflow-step agent may write its
        // produces instance (update_page/validate) and read, but is denied the
        // event tools so it cannot steer the run's phase sequence.
        for denied in ["capture_event", "assign_event"] {
            assert!(
                !WORKFLOW_STEP_TOOLS.contains(&denied),
                "{denied} must be denied to a workflow step"
            );
        }
        for kept in [
            "update_page",
            "validate",
            "list_instances",
            "expand",
            "search",
        ] {
            assert!(WORKFLOW_STEP_TOOLS.contains(&kept), "{kept} must remain");
        }
        // It is exactly the full surface minus the two event tools.
        let expected: Vec<&&str> = ALLOWED_TOOLS
            .iter()
            .filter(|t| **t != "capture_event" && **t != "assign_event")
            .collect();
        let actual: Vec<&&str> = WORKFLOW_STEP_TOOLS.iter().collect();
        assert_eq!(actual, expected);
    }

    /// The event BODY must not ride in the instructions.
    ///
    /// The instructions become `claude --append-system-prompt <string>` and
    /// `codex`'s prompt framing — both argv. Linux caps a single argv string
    /// at 32 pages, so a 220 KB meeting transcript in the instructions made
    /// `spawn` fail with E2BIG and the event permanently undispatchable.
    ///
    /// It is also the right split on its own terms: the system prompt should
    /// carry the PROCEDURE (the skill body), and the user prompt the DATA
    /// (the event payload).
    /// An assigned trigger must name its page IN the instructions.
    ///
    /// Stating it only in the input left the model free to pick, and it did:
    /// two attempts, two invented page ids, both self-reported `ok`, and a
    /// workflow that never converged.
    /// A barrier step must be told its slot, because nothing it can read
    /// carries one — the step id is content-addressed and the material under
    /// review is identical for every skeptic.
    #[test]
    fn a_barrier_step_is_told_the_slot_the_tally_reads() {
        let mut wf = escurel_types::WorkflowProvenance {
            run: "markdown/instances/workflow-run/vfy.md".into(),
            wf_skill: "verify-wf".into(),
            phase: "verify".into(),
            step: "STEP1".into(),
            barrier: "verify".into(),
            over: "markdown/instances/claims/vfy-extract-abc.md".into(),
            vote_index: Some(2),
            harness: String::new(),
        };
        let trigger = |wf: Option<escurel_types::WorkflowProvenance>| Trigger {
            tenant: "acme".into(),
            event_id: "EVT1".into(),
            label_skill: "verify-vote".into(),
            instance_page_id: Some("markdown/instances/verify-vote/vfy-verify-abc.md".into()),
            lineage: crate::Lineage::root("EVT1"),
            workflow: wf,
            content_hash: None,
        };

        let vote = build_instructions(
            &trigger(Some(wf.clone())),
            "SKILLBODY",
            None,
            Autonomy::Auto,
        );
        assert!(vote.contains("`vote_index: 2`"), "the slot: {vote}");
        assert!(
            vote.contains("`claim: vfy-extract-abc`"),
            "the tally key, as the reducer reads it — the last segment of \
             `over`, no directory and no .md: {vote}"
        );
        assert!(vote.contains("verdict"), "{vote}");

        // CONTROL: a non-barrier step of the same workflow is told its run and
        // phase but never invents a slot — a `vote_index` on a page the tally
        // does not read is noise, and a wrong one would skew a barrier.
        wf.vote_index = None;
        let plain = build_instructions(&trigger(Some(wf)), "SKILLBODY", None, Autonomy::Auto);
        assert!(plain.contains("workflow_run:"), "{plain}");
        assert!(!plain.contains("vote_index"), "{plain}");

        // CONTROL: an ordinary event is not a workflow step at all.
        let ordinary = build_instructions(&trigger(None), "SKILLBODY", None, Autonomy::Auto);
        assert!(!ordinary.contains("one step of a workflow"), "{ordinary}");
    }

    #[test]
    fn an_assigned_trigger_names_its_target_page_in_the_instructions() {
        let page = "markdown/instances/deep-research/dr-42.md";
        let mut trigger = Trigger {
            tenant: "acme".into(),
            event_id: "EVT1".into(),
            label_skill: "deep-research".into(),
            instance_page_id: Some(page.to_owned()),
            lineage: crate::Lineage::root("EVT1"),
            workflow: None,
            content_hash: None,
        };
        let assigned = build_instructions(&trigger, "SKILLBODY", None, Autonomy::Auto);
        assert!(
            assigned.contains(page),
            "the assigned page id must be in the instructions: {assigned}"
        );
        assert!(
            assigned.contains("only this page"),
            "and it must be stated as a constraint, not a mention: {assigned}"
        );

        // The page contract, derived from the PAGE id and not from
        // `label_skill` — a workflow board is `workflow-run/<run>.md` while
        // the trigger's skill is the workflow's own. Stamping the trigger's
        // skill there authors an invalid page in the one place it matters
        // most: nothing downstream reads a board that would not validate.
        assert!(
            assigned.contains("`skill: deep-research`") && assigned.contains("`id: dr-42`"),
            "the frontmatter the store demands must be spelled out: {assigned}"
        );

        // The completion protocol: assigning is what ends an auto run, and a
        // run that only writes is re-run until it dead-letters.
        assert!(
            assigned.contains("assign_event"),
            "the auto path must say how a run finishes: {assigned}"
        );

        // …and under review, where the write is a draft, the same page is the
        // draft's target: the constraint must survive the gate text — but the
        // assign instruction must NOT, because a review run has no such verb
        // and the event stays in the inbox for a human on purpose.
        let review = build_instructions(&trigger, "SKILLBODY", None, Autonomy::Review);
        assert!(review.contains(page), "{review}");
        assert!(review.contains("target_page_id"), "{review}");
        assert!(
            !review.contains("call `assign_event`"),
            "a review run must not be told to assign: {review}"
        );
        // …and it must never be pointed at a verb it does not have. Naming
        // `update_page` here cost a real run: the model spent its turns on a
        // tool the packager had already denied it, drafted nothing, and the
        // run reached no verdict at all.
        assert!(
            !review.contains("`update_page`"),
            "a review run has no update_page; the page contract must be \
             stated in terms of create_draft: {review}"
        );
        assert!(review.contains("`create_draft` VALIDATES"), "{review}");
        assert!(
            assigned.contains("`update_page` VALIDATES"),
            "control: the auto path still names its own verb: {assigned}"
        );

        // CONTROL: a trigger with no instance yet must NOT carry the section —
        // there is no page to pin, and telling an agent to write to "only this
        // page" when none was chosen would be a contradiction.
        trigger.instance_page_id = None;
        let unassigned = build_instructions(&trigger, "SKILLBODY", None, Autonomy::Auto);
        assert!(
            !unassigned.contains("only this page"),
            "an unassigned trigger must still be free to create one: {unassigned}"
        );
    }

    #[test]
    fn a_large_event_body_does_not_ride_in_the_instructions() {
        const MAX_ARG_STRLEN: usize = 32 * 4096;
        let trigger = Trigger {
            tenant: "acme".into(),
            event_id: "EVT1".into(),
            label_skill: "meeting".into(),
            instance_page_id: None,
            lineage: crate::Lineage::root("EVT1"),
            workflow: None,
            content_hash: None,
        };
        let event = Event {
            event_id: "EVT1".into(),
            label_skill: "meeting".into(),
            source: "rekorder".into(),
            title: "a workshop".into(),
            body: "x".repeat(220 * 1024),
            ..Event::default()
        };
        let instr = build_instructions(&trigger, "SKILLBODY", Some(&event), Autonomy::Auto);
        assert!(
            instr.len() < MAX_ARG_STRLEN,
            "instructions are {} bytes, over the {MAX_ARG_STRLEN}-byte per-argument \
             limit; spawn would fail with E2BIG",
            instr.len()
        );
        // The payload has to reach the agent somewhere — the input.
        let input = build_input_for_new_instance(&trigger, Some(&event));
        assert!(
            input.contains(&"x".repeat(1024)),
            "the event body must travel in the input instead"
        );
    }

    #[test]
    fn instructions_carry_framing_skill_body_and_event() {
        let trigger = Trigger {
            tenant: "acme".into(),
            event_id: "EVT1".into(),
            label_skill: "note".into(),
            instance_page_id: None,
            lineage: crate::Lineage::root("EVT1"),
            workflow: None,
            content_hash: None,
        };
        let event = Event {
            event_id: "EVT1".into(),
            label_skill: "note".into(),
            source: "manual".into(),
            title: "TITLEMARK".into(),
            body: "BODYMARK".into(),
            ..Event::default()
        };
        let instr = build_instructions(&trigger, "SKILLBODY", Some(&event), Autonomy::Auto);
        assert!(instr.contains("note"));
        assert!(instr.contains("SKILLBODY"));
        assert!(instr.contains("EVT1"));
        assert!(
            instr.contains("TITLEMARK"),
            "the event title is a cheap, bounded reference: {instr}"
        );
        // The BODY belongs in the input, not the instructions — see
        // `a_large_event_body_does_not_ride_in_the_instructions`.
        assert!(
            !instr.contains("BODYMARK"),
            "the event body must NOT be in the instructions: {instr}"
        );
        let input = build_input_for_new_instance(&trigger, Some(&event));
        assert!(
            input.contains("BODYMARK"),
            "event body in the input: {input}"
        );

        // No event record recovered → fall back to the trigger ids.
        let fallback = build_instructions(&trigger, "SKILLBODY", None, Autonomy::Auto);
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
            workflow: None,
            content_hash: None,
        };
        let input = build_input_for_new_instance(&trigger, None);
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
            autonomy: Autonomy::Auto,
            token: SecretString::from("super-secret-token".to_string()),
        };
        let dbg = format!("{ctx:?}");
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("super-secret-token"));
        // token_str() still hands the real secret to explicit callers.
        assert_eq!(ctx.token_str(), "super-secret-token");
    }
}
