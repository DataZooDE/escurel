//! Choosing the agent profile a run executes as (#823).
//!
//! Order: the triggering event's `provenance.agent` request, honoured only
//! when that agent lists the run's skill; else the skill page's own `agent:`
//! default; else no agent (today's behaviour). The runner reads the agent page
//! here to pick the harness, model and concurrency — the GATEWAY reads it again
//! to enforce the scopes, so nothing this module decides can widen them.

use escurel_client::Client;
use escurel_types::ExpandRequest;
use escurel_types::agent_profile::{AGENT_SKILL, AgentProfile, agent_page_id, is_valid_agent_id};

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

/// Resolve the agent for a run of `skill`: `requested` from the event,
/// `default` from the skill page (each an agent id, possibly malformed).
pub async fn choose_agent(
    client: &Client,
    skill: &str,
    requested: Option<&str>,
    default: Option<&str>,
) -> AgentChoice {
    let requested = match requested {
        Some(id) => Some((id, load(client, id).await)),
        None => None,
    };
    let default = match default {
        Some(id) => Some((id, load(client, id).await)),
        None => None,
    };
    decide(skill, requested, default)
}

/// The agent page `id` names, or `None` when there is no usable one.
async fn load(client: &Client, id: &str) -> Option<AgentProfile> {
    if !is_valid_agent_id(id) {
        return None;
    }
    let page = client
        .expand(ExpandRequest {
            page_id: agent_page_id(id),
            ..Default::default()
        })
        .await
        .ok()?;
    let is_agent = page.page.as_ref().is_some_and(|p| p.skill == AGENT_SKILL);
    let archived = page.frontmatter.get("archived").and_then(|v| v.as_bool()) == Some(true);
    (is_agent && !archived).then(|| AgentProfile::from_frontmatter(id, &page.frontmatter))
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
