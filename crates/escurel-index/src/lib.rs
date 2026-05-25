//! DuckDB-backed index for Escurel.
//!
//! Today this crate ships only [`Migrator`], which applies the v1
//! schema (the six tables in `docs/spec/storage.md §DuckDB schema`)
//! to a fresh DuckDB file. Indexer (`update_page`), audit, and
//! rebuild arrive in a later PR.
//!
//! ## Extension policy
//!
//! The schema uses the `vss` HNSW index on `blocks.dense_vec` and a
//! BM25 FTS index on `blocks.body`. Both extensions are "known" to
//! DuckDB, so [`Migrator::up`] turns on
//! `autoinstall_known_extensions` and `autoload_known_extensions`;
//! DuckDB downloads + loads them transparently the first time the
//! DDL references them.
//!
//! Substrate deployments bake the extension binaries into the
//! golden image (`docs/deploy/substrate.md §6`); the auto-install
//! egress is dev-only.

pub mod chat;
pub mod citation;
pub mod indexer;
pub mod query;
pub mod read;
pub mod schema;
pub mod search;
pub mod validate;

pub use chat::{AppendChatMessage, ChatMessage, ChatPage, ListChatMessages};
pub use citation::IndexerCitationLookup;
pub use indexer::{AuditDrift, Indexer, IndexerError, RebuildProgress};
pub use query::{ColumnSchema, QueryError, StoredQueryResult};
pub use read::{
    BlockInfo, Direction, Edge, ExpandedPage, InstanceInfo, OrderDir, PageRef, ResolvedWikilink,
    SkillInfo,
};
pub use schema::Migrator;
pub use search::SearchHit;
pub use validate::{Issue, Severity};
