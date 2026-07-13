//! CLI ↔ server **tool-surface parity guard**.
//!
//! The `escurel` CLI must stay en par with the gateway's tool surface:
//! every tool the server advertises in `tools/list` has to be reachable
//! from a CLI command (agent group or admin group), or be *deliberately*
//! excluded with a documented reason. This test is the ratchet that makes
//! that a merge gate rather than a habit.
//!
//! It drives the **real** gateway (via `escurel-test-support`), reads the
//! live `tools/list`, and reconciles it against the [`COVERAGE`] table
//! below:
//!
//! * a live tool with no table entry fails the test — a new server tool
//!   landed and nobody wired the CLI (or declared it out of scope);
//! * a table entry with no live tool fails the test — a tool was removed
//!   and the table is stale;
//! * every `Agent` / `Admin` entry is proven to exist by invoking
//!   `escurel <path…> --help` and asserting clap accepts the path.
//!
//! When you add a server tool: add a row here. If it gets a CLI command,
//! use `Agent`/`Admin` with the command path; if it is intentionally
//! CLI-less (an MCP/gRPC-twin provisioning tool), use `Excluded` with the
//! reason. Either way the decision is explicit and reviewed.

use assert_cmd::Command;
use escurel_test_support::{AuthMode, EscurelProcess, Opts};
use serde_json::{Value, json};

/// How the CLI covers one server tool.
enum Coverage {
    /// Reachable via an agent-surface command; the slice is the argv path
    /// (e.g. `&["page", "expand"]`).
    Agent(&'static [&'static str]),
    /// Reachable via an `admin` command; the slice is the path *after*
    /// `admin` (e.g. `&["tenant", "create"]`).
    Admin(&'static [&'static str]),
    /// Intentionally not surfaced in the CLI. The string documents why.
    Excluded(&'static str),
}

use Coverage::{Admin, Agent, Excluded};

/// The single source of truth for CLI ↔ tool parity. Keep it in sync with
/// `tools_list_payload()` in `escurel-server/src/mcp.rs`.
const COVERAGE: &[(&str, Coverage)] = &[
    // --- agent surface: every one MUST have an agent CLI command --------
    ("list_skills", Agent(&["skill", "list"])),
    ("list_instances", Agent(&["instance", "list"])),
    ("resolve", Agent(&["resolve"])),
    ("expand", Agent(&["page", "expand"])),
    ("fetch_blob", Agent(&["page", "blob"])),
    ("neighbours", Agent(&["link", "neighbours"])),
    ("search", Agent(&["search"])),
    ("run_stored_query", Agent(&["query", "run"])),
    ("query_instance", Agent(&["query", "instance"])),
    ("validate", Agent(&["page", "validate"])),
    ("update_page", Agent(&["page", "update"])),
    ("append_message", Agent(&["chat", "append"])),
    ("list_messages", Agent(&["chat", "list"])),
    ("capture_event", Agent(&["event", "capture"])),
    ("list_inbox", Agent(&["event", "inbox"])),
    ("list_events", Agent(&["event", "list"])),
    ("list_snapshots", Agent(&["page", "snapshots"])),
    ("assign_event", Agent(&["event", "assign"])),
    ("open_session", Agent(&["session", "open"])),
    ("apply_op", Agent(&["session", "apply"])),
    ("close_session", Agent(&["session", "close"])),
    // --- admin surface with a CLI command -------------------------------
    ("admin_quota", Admin(&["quota"])),
    ("admin_audit", Admin(&["audit"])),
    ("admin_delete_chat_history", Admin(&["delete-chat-history"])),
    ("attach_external", Admin(&["attach-external"])),
    ("embedding_reload", Admin(&["embedding-reload"])),
    ("rebuild", Admin(&["rebuild"])),
    ("compact_lanes", Admin(&["compact-lanes"])),
    ("tenant_create", Admin(&["tenant", "create"])),
    ("tenant_list", Admin(&["tenant", "list"])),
    ("tenant_get", Admin(&["tenant", "get"])),
    ("tenant_update", Admin(&["tenant", "update"])),
    ("tenant_delete", Admin(&["tenant", "delete"])),
    ("tenant_export", Admin(&["tenant", "export"])),
    ("tenant_import", Admin(&["tenant", "import"])),
    ("export_pack", Admin(&["pack", "export"])),
    // --- admin/ops MCP-twins deliberately kept off the CLI --------------
    // These mirror the EscurelAdmin gRPC / MCP provisioning surface and
    // are driven from the substrate/BFF, not the operator CLI. If one ever
    // earns a CLI command, move its row up to `Admin(…)`.
    (
        "admin_webhook_deliveries",
        Excluded("ops introspection; MCP-only"),
    ),
    (
        "admin_index_query",
        Excluded("ops/debug table peek; MCP-only"),
    ),
    (
        "admin_list_lanes",
        Excluded("ops lane introspection; MCP-only"),
    ),
    (
        "admin_lane_keys",
        Excluded("ops lane introspection; MCP-only"),
    ),
    (
        "admin_lane_blob",
        Excluded("ops lane introspection; MCP-only"),
    ),
    (
        "add_group_member",
        Excluded("RBAC provisioning; MCP/gRPC-twin"),
    ),
    (
        "remove_group_member",
        Excluded("RBAC provisioning; MCP/gRPC-twin"),
    ),
    (
        "list_group_members",
        Excluded("RBAC provisioning; MCP/gRPC-twin"),
    ),
    (
        "register_credential",
        Excluded("secret provisioning; MCP/gRPC-twin"),
    ),
    (
        "list_credentials",
        Excluded("secret provisioning; MCP/gRPC-twin"),
    ),
    (
        "delete_credential",
        Excluded("secret provisioning; MCP/gRPC-twin"),
    ),
    (
        "validate_bindings",
        Excluded("provisioning preflight; MCP-only"),
    ),
    (
        "create_sql_instance",
        Excluded("sql_view provisioning; MCP/gRPC-twin"),
    ),
    (
        "register_endpoint",
        Excluded("remote-backend provisioning; MCP-only"),
    ),
    (
        "list_endpoints",
        Excluded("remote-backend provisioning; MCP-only"),
    ),
    (
        "delete_endpoint",
        Excluded("remote-backend provisioning; MCP-only"),
    ),
    (
        "validate_endpoints",
        Excluded("provisioning preflight; MCP-only"),
    ),
    (
        "create_remote_instance",
        Excluded("remote provisioning; MCP/gRPC-twin"),
    ),
    (
        "write_instance",
        Excluded("event-sourced write op; MCP-only"),
    ),
];

/// Read the live `tools/list` names from a running gateway.
async fn live_tool_names(base_url: &str) -> Vec<String> {
    let body: Value = reqwest::Client::new()
        .post(format!("{base_url}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("POST tools/list")
        .json()
        .await
        .expect("decode tools/list");
    body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_owned())
        .collect()
}

/// `escurel <path…> --help` must exit 0 — proof the command path is wired
/// into clap. `--help` short-circuits before any network call.
fn assert_command_wired(path: &[&str], tool: &str) {
    let mut cmd = Command::cargo_bin("escurel").expect("escurel binary");
    cmd.args(path).arg("--help");
    let out = cmd.output().expect("run --help");
    assert!(
        out.status.success(),
        "tool `{tool}` maps to CLI path `escurel {}` but that command is not \
         wired (clap rejected it):\n{}",
        path.join(" "),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_server_tool_is_covered_by_the_cli() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: Default::default(),
    })
    .await;

    let live: std::collections::BTreeSet<String> = live_tool_names(process.base_url())
        .await
        .into_iter()
        .collect();
    let table: std::collections::BTreeSet<String> =
        COVERAGE.iter().map(|(name, _)| name.to_string()).collect();

    // 1. No unmapped server tool: a new tool must be classified here.
    let unmapped: Vec<&String> = live.difference(&table).collect();
    assert!(
        unmapped.is_empty(),
        "server advertises tool(s) with no coverage entry: {unmapped:?}\n\
         → add a row to COVERAGE in cli_parity.rs: give it an Agent/Admin \
         CLI command, or Excluded(\"reason\") if it is intentionally CLI-less.",
    );

    // 2. No stale table row: a removed tool must be dropped from the table.
    let stale: Vec<&String> = table.difference(&live).collect();
    assert!(
        stale.is_empty(),
        "COVERAGE lists tool(s) the server no longer advertises: {stale:?}\n\
         → remove the stale row(s) from cli_parity.rs.",
    );

    // 3. Every mapped command actually exists in the CLI.
    for (tool, coverage) in COVERAGE {
        match coverage {
            Agent(path) => assert_command_wired(path, tool),
            Admin(path) => {
                let mut full = vec!["admin"];
                full.extend_from_slice(path);
                assert_command_wired(&full, tool);
            }
            // An exclusion is a deliberate, reviewed decision — it must
            // carry a reason so the next reader knows why the tool has no
            // CLI command.
            Excluded(reason) => assert!(
                !reason.is_empty(),
                "tool `{tool}` is Excluded but carries no reason — document why it is CLI-less",
            ),
        }
    }

    process.shutdown().await;
}
