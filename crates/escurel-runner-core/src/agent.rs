//! Choosing the agent profile a run executes as (#823).
//!
//! Order: the triggering event's `provenance.agent` request, honoured only
//! when that agent lists the run's skill; else the skill page's own `agent:`
//! default; else no agent (today's behaviour). The runner reads the agent page
//! here to pick the harness, model and concurrency — the GATEWAY reads it again
//! to enforce the scopes, so nothing this module decides can widen them.

use escurel_client::Client;
use escurel_types::ExpandRequest;
use escurel_types::agent_profile::{
    AgentProfile, agent_page_id, is_usable_agent_page, is_valid_agent_id,
};

/// The agent a run executes as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentChoice {
    /// No agent requested or declared: the run keeps the runner's usual
    /// per-run token.
    None,
    Agent(AgentProfile),
    /// The skill declares a default agent that cannot run it (missing, or not
    /// listing the skill). The run is refused rather than given a broader
    /// token than its author asked for.
    Refused(String),
}

impl AgentChoice {
    /// The chosen profile, if any.
    #[must_use]
    pub fn profile(&self) -> Option<&AgentProfile> {
        match self {
            Self::Agent(p) => Some(p),
            _ => None,
        }
    }
}

/// The skill's contract and the agent a run of `trigger` executes as — THE
/// resolution, shared by a fresh dispatch and by crash recovery so both read
/// the same sources: the event's `provenance.agent` request, then the skill's
/// default (read off its page). A workflow step runs as its requester and a
/// chat turn as its asker, so neither takes an agent.
///
/// # Errors
/// When the skill page or an agent page could not be READ (a transport or
/// gateway error, as opposed to a page that is absent). The caller must not
/// run on a guess: an unread skill contract would drop a declared default
/// agent and run on the runner's own token, and an unread agent page would
/// dead-letter the run as if the agent did not exist.
pub async fn resolve_run_agent(
    client: &Client,
    trigger: &crate::Trigger,
) -> Result<(Option<crate::SkillContract>, AgentChoice), escurel_client::Error> {
    let contract = crate::skill_contract(client, &trigger.label_skill).await?;
    let agent = agent_for_trigger(client, trigger, contract.as_ref()).await?;
    Ok((contract, agent))
}

/// [`resolve_run_agent`] with the skill contract already read.
///
/// # Errors
/// When an agent page could not be read (see [`resolve_run_agent`]).
pub async fn agent_for_trigger(
    client: &Client,
    trigger: &crate::Trigger,
    contract: Option<&crate::SkillContract>,
) -> Result<AgentChoice, escurel_client::Error> {
    if trigger.workflow.is_some() || trigger.chat.is_some() {
        return Ok(AgentChoice::None);
    }
    choose_agent(
        client,
        &trigger.label_skill,
        trigger.agent.as_deref(),
        contract.and_then(|c| c.agent.as_deref()),
    )
    .await
}

/// Resolve the agent for a run of `skill`: `requested` from the event,
/// `default` from the skill page (each an agent id, possibly malformed).
///
/// # Errors
/// When an agent page could not be read (absent is not an error).
pub async fn choose_agent(
    client: &Client,
    skill: &str,
    requested: Option<&str>,
    default: Option<&str>,
) -> Result<AgentChoice, escurel_client::Error> {
    let requested = match requested {
        Some(id) => Some((id, load(client, id).await?)),
        None => None,
    };
    let default = match default {
        Some(id) => Some((id, load(client, id).await?)),
        None => None,
    };
    Ok(decide(skill, requested, default))
}

/// The agent page `id` names, `None` when there is no usable one (absent,
/// archived, not an `agent` instance, or an id no page can carry).
///
/// # Errors
/// When the page could not be READ: that is not "no such agent".
async fn load(client: &Client, id: &str) -> Result<Option<AgentProfile>, escurel_client::Error> {
    if !is_valid_agent_id(id) {
        return Ok(None);
    }
    let page = client
        .expand(ExpandRequest {
            page_id: agent_page_id(id),
            ..Default::default()
        })
        .await?;
    let usable = page.page.as_ref().is_some_and(|p| {
        is_usable_agent_page(&p.skill, p.page_kind == "instance", &page.frontmatter)
    });
    Ok(usable.then(|| AgentProfile::from_frontmatter(id, &page.frontmatter)))
}

/// The pure half of [`choose_agent`].
#[must_use]
pub fn decide(
    skill: &str,
    requested: Option<(&str, Option<AgentProfile>)>,
    default: Option<(&str, Option<AgentProfile>)>,
) -> AgentChoice {
    if let Some((id, found)) = requested {
        match found {
            Some(p) if p.lists_skill(skill) => return AgentChoice::Agent(p),
            // A request is a narrowing the requester asked for; one that cannot
            // be honoured leaves the skill's own choice in force.
            _ => tracing::warn!(
                target: "escurel_runner",
                agent = %id,
                %skill,
                "the event asks for an agent that does not exist or does not list this skill; \
                 ignoring the request"
            ),
        }
    }
    match default {
        None => AgentChoice::None,
        Some((_, Some(p))) if p.lists_skill(skill) => AgentChoice::Agent(p),
        Some((id, Some(_))) => AgentChoice::Refused(format!(
            "skill `{skill}` names agent `{id}` as its default, but that agent does not list \
             the skill; refusing rather than running it without its agent"
        )),
        Some((id, None)) => AgentChoice::Refused(format!(
            "skill `{skill}` names agent `{id}` as its default, but there is no such agent \
             page; refusing rather than running it without its agent"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str, skills: &[&str]) -> AgentProfile {
        let links: Vec<String> = skills.iter().map(|s| format!("[[skill::{s}]]")).collect();
        AgentProfile::from_frontmatter(id, &json!({ "skills": links }))
    }

    #[test]
    fn a_request_for_an_agent_listing_the_skill_wins_over_the_default() {
        let got = decide(
            "note",
            Some(("a", Some(agent("a", &["note"])))),
            Some(("b", Some(agent("b", &["note"])))),
        );
        assert_eq!(got.profile().map(|p| p.id.as_str()), Some("a"));
    }

    #[test]
    fn an_unhonourable_request_falls_back_to_the_default() {
        for requested in [("a", Some(agent("a", &["other"]))), ("ghost", None)] {
            let got = decide(
                "note",
                Some(requested),
                Some(("b", Some(agent("b", &["note"])))),
            );
            assert_eq!(got.profile().map(|p| p.id.as_str()), Some("b"));
        }
        assert_eq!(
            decide("note", Some(("ghost", None)), None),
            AgentChoice::None
        );
    }

    /// A gateway that does not answer (a real refused connection, not a stub: the boundary under
    /// test is the runner's handling of a READ error). Before crew review PR-1 F5, an unread skill
    /// page read as "no default agent" (the run went wide on the runner's token) and an unread agent
    /// page as "no such agent" (a permanent dead-letter with a false reason). Both are errors now.
    async fn unreachable_gateway() -> Client {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Client::connect(
            &format!("http://127.0.0.1:{port}"),
            crate::SecretString::from("t".to_owned()),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_unreadable_skill_page_is_an_error_not_no_agent() {
        let client = unreachable_gateway().await;
        let event = escurel_types::Event {
            label_skill: "note".to_owned(),
            ..Default::default()
        };
        let trigger = crate::Trigger::from_event(&event, "acme");
        assert!(resolve_run_agent(&client, &trigger).await.is_err());
    }

    #[tokio::test]
    async fn an_unreadable_agent_page_is_an_error_not_a_refusal() {
        let client = unreachable_gateway().await;
        assert!(
            choose_agent(&client, "note", None, Some("scribe"))
                .await
                .is_err()
        );
        assert!(
            choose_agent(&client, "note", Some("scribe"), None)
                .await
                .is_err()
        );
        // No agent asked for: nothing to read, no error.
        assert_eq!(
            choose_agent(&client, "note", None, None).await.unwrap(),
            AgentChoice::None
        );
    }

    #[test]
    fn no_request_and_no_default_is_no_agent() {
        assert_eq!(decide("note", None, None), AgentChoice::None);
    }

    #[test]
    fn a_default_that_cannot_run_the_skill_refuses_the_run() {
        assert!(matches!(
            decide("note", None, Some(("b", Some(agent("b", &["other"]))))),
            AgentChoice::Refused(_)
        ));
        assert!(matches!(
            decide("note", None, Some(("ghost", None))),
            AgentChoice::Refused(_)
        ));
    }
}
