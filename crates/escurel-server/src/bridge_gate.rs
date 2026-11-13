//! The chat-bridge gate (crew security F2): what a bridge token may do.
//!
//! A chat bridge signs for anyone it answers (`escurel-auth`'s bridge trust
//! entry), so a bridge token is a relay for one chat turn, not a stand-in for
//! the person on the agent surface. Without this gate, holding the bridge key
//! meant reading whatever the named person may read and writing their records
//! directly wherever a skill lands machine writes (`autonomy: auto`). The
//! allow-list is the bridge's own turn (`escurel-chat-bridge`'s `Bridge::work`)
//! and nothing else; every refusal is `-32003 forbidden`.
//!
//! The non-tool surfaces (`/ingest`, `/blob`, `/ws`) are refused outright in
//! `role_gate::surface_refusal`: the bridge uses none of them.

use escurel_index::Indexer;
use serde_json::Value;

/// The tools a bridge token may call — exactly what a turn needs:
///
/// * `whoami`: names the credential's own subject; reveals nothing the token
///   does not already say, and is how an operator checks a bridge credential.
/// * `update_page`: create-if-absent of the person's conversation page, an
///   instance of a `chat: {page_bound: true}` skill. Never an edit.
/// * `append_message`: the person's own (`role: user`) turn into a page-bound
///   chat group; the answer is the run's to write.
/// * `list_messages`: reading that group back for the run's answer.
/// * `capture_event`: the chat event the runner answers, under the configured
///   chat label only — the label picks which skill runs.
/// * `ingest_upload`: the turn's attachments, ingested as the person.
pub(crate) const BRIDGE_TOOLS: &[&str] = &[
    "whoami",
    "update_page",
    "append_message",
    "list_messages",
    "capture_event",
    "ingest_upload",
];

/// Why a bridge token may not make this call, or `None` when it may.
///
/// # Errors
/// The skill registry could not be read (`update_page`'s page-bound check).
pub(crate) async fn refusal(
    tool: &str,
    args: &Value,
    chat_skill: &str,
    indexer: Option<&Indexer>,
) -> Result<Option<String>, escurel_index::IndexerError> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str);
    let why = match tool {
        _ if !BRIDGE_TOOLS.contains(&tool) => {
            format!("a chat bridge's token may not call `{tool}`")
        }
        "update_page" => {
            if arg("base_sha256") != Some("") {
                "a chat bridge only creates a conversation page (`base_sha256: \"\"`), \
                 never edits one"
                    .to_owned()
            } else if !page_bound_instance(arg("page_id").unwrap_or_default(), indexer).await? {
                "a chat bridge only creates instances of a `chat: {page_bound: true}` skill"
                    .to_owned()
            } else {
                return Ok(None);
            }
        }
        "append_message" | "list_messages" => {
            if !escurel_index::is_page_chat(arg("chat_group_id").unwrap_or_default()) {
                "a chat bridge only reaches page-bound chat groups (`page-chat:`)".to_owned()
            } else if tool == "append_message" && arg("role") != Some("user") {
                "a chat bridge appends the person's turn (`role: user`) only".to_owned()
            } else {
                return Ok(None);
            }
        }
        "capture_event" if arg("label_skill") != Some(chat_skill) => format!(
            "a chat bridge captures under the chat label `{chat_skill}` only \
             (ESCUREL_AUTH_BRIDGE_CHAT_SKILL)"
        ),
        _ => return Ok(None),
    };
    Ok(Some(why))
}

/// `page_id` is `markdown/instances/<skill>/<id>.md` and `<skill>` declares
/// `chat: {page_bound: true}`.
async fn page_bound_instance(
    page_id: &str,
    indexer: Option<&Indexer>,
) -> Result<bool, escurel_index::IndexerError> {
    let (Some((skill, _)), Some(indexer)) = (
        escurel_index::backend::rows::split_instance_page_id(page_id),
        indexer,
    ) else {
        return Ok(false);
    };
    Ok(indexer
        .list_skills()
        .await?
        .iter()
        .any(|s| s.id == skill && s.chat_page_bound))
}
