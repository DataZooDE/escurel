//! The complete tool → execution-label map, pinned.
//!
//! `tool_execution_labels.rs` asserts that every tool carries a *valid* label
//! and spot-checks a handful. That is enough to catch a missing label and not
//! enough to catch a changed one: 68 tools are advertised and it names 8.
//!
//! This file pins all of them. Its first job was to make R2 of
//! `docs/notes/complexity-reduction-plan.md` — folding `DETERMINISTIC_TOOLS`
//! into the tool definitions so a tool cannot exist without a label — provably
//! behaviour-preserving rather than merely believed to be. It keeps earning
//! its place afterwards: the label is a contract a per-phase tool surface
//! filters on ("the LLM never does critical arithmetic"), so a tool silently
//! changing sides is a security-relevant regression that no other test sees.
//!
//! **This list is a golden expectation, not a source of truth.** The labels
//! live at the tool definitions in `mcp/schema.rs`; this file only remembers
//! what they were. Updating it when the surface changes is expected — but it
//! should be a deliberate line in a diff, reviewed as a decision, which is the
//! whole point. If you find yourself editing this file to make a build pass
//! without knowing which tool moved, stop: that is the regression it exists
//! to show you. (codex review flagged the risk that a second complete list
//! reads as re-introducing the duplication R2 removed — it does not, because
//! nothing consumes it.)
//!
//! Real gateway, real `/mcp` `tools/list`. No mocks.

use std::collections::BTreeSet;

use escurel_test_support::{AuthMode, EscurelProcess, Opts};
use serde_json::{Value, json};

/// `name:execution` for every advertised tool, sorted.
const EXPECTED: &[&str] = &[
    "abandoned_paths:orchestration",
    "add_group_member:orchestration",
    "admin_audit:deterministic",
    "admin_delete_chat_history:orchestration",
    "admin_index_query:deterministic",
    "admin_lane_blob:deterministic",
    "admin_lane_keys:deterministic",
    "admin_list_lanes:deterministic",
    "admin_quota:orchestration",
    "admin_webhook_deliveries:orchestration",
    "append_message:orchestration",
    "apply_op:orchestration",
    "assign_event:orchestration",
    "attach_external:orchestration",
    "capture_event:orchestration",
    "close_session:orchestration",
    "compact_lanes:orchestration",
    "create_remote_instance:orchestration",
    "create_sql_instance:orchestration",
    "delete_credential:orchestration",
    "delete_endpoint:orchestration",
    "delete_page:orchestration",
    "embedding_reload:orchestration",
    "expand:deterministic",
    "expectation_drift:orchestration",
    "export_pack:deterministic",
    "fetch_blob:deterministic",
    "import_pack:orchestration",
    "list_credentials:deterministic",
    "list_endpoints:deterministic",
    "list_events:deterministic",
    "list_group_members:deterministic",
    "list_inbox:deterministic",
    "list_instances:deterministic",
    "list_messages:deterministic",
    "list_packs:deterministic",
    "list_skills:deterministic",
    "list_snapshots:deterministic",
    "move_page:orchestration",
    "neighbours:deterministic",
    "open_session:orchestration",
    "provenance_ancestry:orchestration",
    "provenance_path:orchestration",
    "publish_snapshot:orchestration",
    "purge_page:orchestration",
    "query_instance:deterministic",
    "rebase_pack:orchestration",
    "rebuild:orchestration",
    "register_credential:orchestration",
    "register_endpoint:orchestration",
    "remove_group_member:orchestration",
    "resolve:deterministic",
    "run_stored_query:deterministic",
    "search:deterministic",
    "submit_promotion:orchestration",
    "tenant_create:orchestration",
    "tenant_delete:orchestration",
    "tenant_export:deterministic",
    "tenant_get:deterministic",
    "tenant_import:orchestration",
    "tenant_list:deterministic",
    "tenant_update:orchestration",
    "unsubscribe_pack:orchestration",
    "update_page:orchestration",
    "validate:deterministic",
    "validate_bindings:orchestration",
    "validate_endpoints:orchestration",
    "write_instance:orchestration",
];

#[tokio::test]
async fn the_complete_tool_label_map_is_unchanged() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");

    let actual: BTreeSet<String> = body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| {
            format!(
                "{}:{}",
                t["name"].as_str().unwrap_or("?"),
                t["execution"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let expected: BTreeSet<String> = EXPECTED.iter().map(|s| (*s).to_owned()).collect();

    let added: Vec<&String> = actual.difference(&expected).collect();
    let removed: Vec<&String> = expected.difference(&actual).collect();

    assert!(
        added.is_empty() && removed.is_empty(),
        "the tool/label map moved.\n  unexpected: {added:?}\n  missing:    {removed:?}\n\
         If this is intended, update EXPECTED in the same commit — and check \
         that nothing crossed the deterministic/orchestration line by accident."
    );
    p.shutdown().await;
}
