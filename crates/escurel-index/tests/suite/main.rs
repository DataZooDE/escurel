//! The crate's single integration-test binary.
//!
//! Cargo compiles every `tests/*.rs` file into its own executable, each
//! statically linking the whole dependency graph (DuckDB included). Files
//! that sit *inside* `tests/suite/` are not targets in their own right, so
//! declaring them as modules here collapses them into one binary.
//!
//! Adding a test file: put it in `tests/suite/` and add its `mod` line
//! below. A file that is not listed here is silently not compiled — nothing
//! warns you about it.

mod acl;
mod as_of;
mod backend_registry;
mod chat_messages;
mod citation_lookup;
mod contextual_retrieval;
mod crash_recovery;
mod credentials;
mod delete_page;
mod document_rebuild;
mod ducklake_adopt;
mod ducklake_adopt_live;
mod ducklake_append_lake_live;
mod ducklake_chat_live;
mod ducklake_crdt_live;
mod ducklake_events_live;
mod ducklake_publish;
mod ducklake_publish_live;
mod ducklake_spikes_live;
mod duckpgq_spike;
mod embed_lock;
mod endpoints;
mod events;
mod frontmatter_links;
mod historical_expand;
mod index_roundtrip;
mod kreuzberg_extract;
mod live_inspect;
mod merge_from_attached;
mod migrate;
mod neighbours;
mod no_payload_in_catalog_live;
mod query_instance;
mod read_tools;
mod rerank;
mod resolve_expand;
mod run_stored_query;
mod scenarios;
mod search;
mod seed;
mod seed_events;
mod sql_bindings;
mod sql_view_backend;
mod sql_view_postgres;
mod two_pass;
mod validate;
mod write_acl;
mod write_attribution;
mod write_document_blocks;
mod writer_lease_live;
