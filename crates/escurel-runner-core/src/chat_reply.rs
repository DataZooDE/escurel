//! The assistant turn of a chat run (governed chat agent, HLD §5.2, ADR C-1
//! and C-2): the RUNNER appends exactly one reply per run, with the run's own
//! token, so the gateway's reply rule (#785) accepts it and stamps `run_id`,
//! `event_id` and `actor` from the claims. The harness never posts.
//!
//! The `msg_id` is `run:<run_id>:reply`: a retried append — or a retried
//! attempt of the same run — echoes the stored row instead of adding one.
//! Every terminal gets a turn: an answer, or one sentence saying why there is
//! none (FR-T5), so a conversation never waits forever.

use escurel_client::{AppendMessageRequest, Client, ListDraftsRequest, ResolveRequest};
use secrecy::SecretString;
use serde_json::json;

use crate::{ChatContext, RunClaims, TokenSource, Trigger, chat_run_token};

/// Most links a reply's `cited` metadata resolves.
const MAX_CITED: usize = 20;

/// How a chat run ended, as its assistant turn reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatOutcome {
    /// The run finished; the text is the harness's answer.
    Answered(String),
    /// The run failed; the reason class (`harness_error`, `timeout`,
    /// `budget`, `chat_requires_run_tokens`).
    Failed(&'static str),
    /// Cancelled through run-control.
    Cancelled,
}

impl ChatOutcome {
    /// Classify a failed run from its last error text.
    #[must_use]
    pub fn failed_from(error: Option<&str>) -> Self {
        let e = error.unwrap_or_default();
        Self::Failed(if e.contains(crate::CHAT_REQUIRES_RUN_TOKENS) {
            crate::CHAT_REQUIRES_RUN_TOKENS
        } else if e.contains("abandoned") {
            "timeout"
        } else if e.contains("budget") {
            "budget"
        } else {
            "harness_error"
        })
    }

    /// `ok`, `failed` or `cancelled` — the turn's `metadata.status`.
    #[must_use]
    pub fn status(&self) -> &'static str {
        match self {
            Self::Answered(_) => "ok",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn content(&self) -> String {
        match self {
            Self::Answered(text) => text.clone(),
            Self::Failed("timeout") => "I couldn't finish: the answer took too long.".to_owned(),
            Self::Failed("budget") => "I couldn't finish: the run hit its budget.".to_owned(),
            Self::Failed(crate::CHAT_REQUIRES_RUN_TOKENS) => {
                "I couldn't answer: the agent runner is not set up for chat \
                 (it cannot mint run tokens)."
                    .to_owned()
            }
            Self::Failed(_) => "I couldn't finish: the agent run failed.".to_owned(),
            Self::Cancelled => "I stopped: the run was cancelled.".to_owned(),
        }
    }
}

/// The page ids behind the `[[skill::id]]` links in `text`, as the asker
/// resolves them; links that do not resolve are skipped.
async fn cited(client: &Client, text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for link in text.split("[[").skip(1).filter_map(|s| s.split_once("]]")) {
        let target = link.0.split('|').next().unwrap_or_default().trim();
        if target.is_empty() || out.len() >= MAX_CITED {
            continue;
        }
        if let Ok(resolved) = client
            .resolve(ResolveRequest {
                wikilink: format!("[[{target}]]"),
                ..Default::default()
            })
            .await
            && let Some(page) = resolved.page
            && !out.contains(&page.page_id)
        {
            out.push(page.page_id);
        }
    }
    out
}

/// Append the run's assistant turn. Best-effort by contract: the run's
/// terminal is already recorded; a refused append is logged (no content),
/// retried once, and otherwise left — the person sees no answer rather than
/// a wrong one.
pub async fn post_chat_reply(
    runner: &Client,
    gateway_url: &str,
    tokens: &TokenSource,
    trigger: &Trigger,
    turn: &ChatContext,
    run: &RunClaims,
    outcome: &ChatOutcome,
) {
    let token = match chat_run_token(turn, tokens, Some(run)) {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                run_id = %run.run_id,
                chat_group_id = %turn.chat_group_id,
                error = %e,
                "chat: no run token, so no assistant turn can be posted"
            );
            return;
        }
    };
    let client = match Client::connect(gateway_url, SecretString::from(token)).await {
        Ok(c) => c.with_run_id(&run.run_id),
        Err(e) => {
            tracing::error!(target: "escurel_runner", run_id = %run.run_id, error = %e,
                "chat: could not build the run-token client");
            return;
        }
    };
    // The drafts this run left for review (the runner's own read: it sees
    // every open draft, the asker's included).
    let drafts: Vec<String> = runner
        .list_drafts(ListDraftsRequest { limit: 200 })
        .await
        .map(|r| {
            r.drafts
                .into_iter()
                // Named by the event, or stamped from the run token's root
                // claim — a chat turn is always a lineage root, and an agent
                // is not told to pass `event_id`.
                .filter(|d| d.event_id == trigger.event_id || d.root_event_id == trigger.event_id)
                .map(|d| d.draft_id)
                .collect()
        })
        .unwrap_or_default();
    let content = outcome.content();
    let cited = match outcome {
        ChatOutcome::Answered(text) => cited(&client, text).await,
        _ => Vec::new(),
    };
    let mut metadata = json!({
        "status": outcome.status(),
        "drafts": drafts,
        "cited": cited,
        "user_msg_id": turn.user_msg_id,
    });
    if let ChatOutcome::Failed(reason) = outcome {
        metadata["reason"] = json!(reason);
    }
    let request = AppendMessageRequest {
        chat_group_id: turn.chat_group_id.clone(),
        role: "assistant".to_owned(),
        content,
        msg_id: format!("run:{}:reply", run.run_id),
        metadata,
        embed: true,
        ..Default::default()
    };
    for attempt in 1..=2 {
        match client.append_message(request.clone()).await {
            Ok(_) => return,
            Err(e) => tracing::error!(
                target: "escurel_runner",
                run_id = %run.run_id,
                chat_group_id = %turn.chat_group_id,
                attempt,
                error = %e,
                "chat: appending the assistant turn failed"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_classified_from_its_error() {
        assert_eq!(
            ChatOutcome::failed_from(Some("run attempt exceeded 120s and was abandoned")),
            ChatOutcome::Failed("timeout")
        );
        assert_eq!(
            ChatOutcome::failed_from(Some("chat_requires_run_tokens: …")),
            ChatOutcome::Failed("chat_requires_run_tokens")
        );
        assert_eq!(
            ChatOutcome::failed_from(None),
            ChatOutcome::Failed("harness_error")
        );
    }
}
