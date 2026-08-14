//! The crate's shared integration-test binary.
//!
//! Cargo compiles every `tests/*.rs` file into its own executable, each
//! statically linking the whole dependency graph (DuckDB included). Files
//! that sit *inside* `tests/suite/` are not targets in their own right, so
//! declaring them as modules here collapses them into one binary.
//!
//! Adding a test file: put it in `tests/suite/` and add its `mod` line
//! below. A file that is not listed here is silently not compiled — nothing
//! warns you about it.
//!
//! Two files deliberately stay OUT of this binary, as their own targets,
//! because they assert on the *process-global* `tracing` subscriber and so
//! genuinely need process isolation:
//!
//! * `tests/logs_json.rs` installs its own subscriber via
//!   `set_global_default(..).expect(..)`. Any sibling test that boots a
//!   gateway first wins the install (`serve()` -> `install_telemetry`), and
//!   the `expect` would panic.
//! * `tests/telemetry_filter.rs` asserts `LevelFilter::current() == INFO`,
//!   which only holds when `serve()`'s `init_telemetry` is the *first*
//!   subscriber installed in the process.
//!
//! They are also mutually exclusive with each other, so no amount of
//! ordering merges them. Do not move them in here.

mod admin_mcp_tools;
mod admin_publish;
mod auth_quota;
mod autonomy;
mod backend_read_limits;
mod binary_boots;
mod blob_route;
mod chat_acl;
mod chat_idempotency;
mod crm_demo_backends;
mod delete_page;
mod dispatch_aliases;
mod document_ingestion;
mod error_data;
mod event_acl;
mod event_pagination;
mod events;
mod fusion_acl;
mod group_members_acl;
mod health;
mod hlc_single_authority;
mod index_backend;
mod ingest_audio;
mod ingest_blob_quota;
mod ingest_gate_parity;
mod ingest_owner_scope;
mod ingest_webhook;
mod instance_acl;
mod instance_scoped_acl;
mod instances_pagination;
mod layer_read_only;
mod list_skills_acl;
mod mcp;
mod mcp_admin_tools;
mod mcp_lifecycle;
mod mcp_session_tools;
mod meta_skill;
mod metrics_real;
mod multi_issuer_groups;
mod openapi_surface;
mod pack_export;
mod pack_import;
mod pack_rebase;
mod pack_rebase_resumable;
mod pack_unsubscribe;
mod page_write_events;
mod project_memory_pack;
mod project_memory_subprojects;
mod promotion_gate;
mod provenance_analytics;
mod provenance_ancestry;
mod provenance_path;
mod query_instance_tools;
mod reader_role;
mod reader_role_chat;
mod reader_role_crdt;
mod reader_role_events;
mod remote_backend_tools;
mod schema_ergonomics;
mod self_packaging;
mod serve_demo;
mod session_commit_writes_through;
mod shadow_merge;
mod skill_doc_parity;
mod skill_params;
mod snapshot_refresh;
mod sql_creds;
mod sql_validate;
mod sql_view_tools;
mod stored_query_acl;
mod tool_execution_labels;
mod tool_label_map;
mod tool_registry_conformance;
mod tools_list_scope;
mod update_page_automerge;
mod update_page_concurrency;
mod update_page_strict_cas;
mod validate_tool;
mod webhook;
mod write_acl;
mod write_attribution;
mod write_origin_metrics;
mod writer_lease;
mod ws;
mod ws_attach_acl;
mod ws_broadcast;
mod ws_event_subscribe;
mod ws_session;
