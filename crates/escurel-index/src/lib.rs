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

pub mod acl;
pub mod backend;
pub mod chat;
pub mod citation;
pub mod crdt_history;
pub mod creds;
pub mod events;
pub mod filter;
pub mod groups;
pub mod indexer;
pub mod meta_skill;
pub mod query;
pub mod read;
pub mod schema;
pub mod search;
pub mod validate;

pub use acl::AclCaller;
pub use backend::{
    BackendBinding, BackendCtx, BackendKind, BackendRegistry, Capabilities, InstanceBackend,
    MarkdownBackend, Materialized, SearchMode, SqlConnector, SqlViewBackend, SqlViewBinding,
    SqlViewError,
};
pub use chat::{AppendChatMessage, ChatMessage, ChatPage, ListChatMessages};
pub use citation::IndexerCitationLookup;
pub use creds::{CredentialInfo, CredentialRecord};
pub use events::{EVENTS_MAX_LIMIT, EventInfo, NewEvent};
pub use groups::GroupMember;
pub use indexer::{
    AuditDrift, Indexer, IndexerError, RebuildProgress, derive_attach_alias, is_safe_attach_source,
    is_valid_attach_alias,
};
pub use meta_skill::{META_SKILL_ID, META_SKILL_MD, META_SKILL_PAGE_ID};
pub use query::{ColumnSchema, INSPECTABLE_TABLES, QueryError, StoredQueryResult};
pub use read::{
    AclPolicy, BlockInfo, Direction, Edge, ExpandedPage, InstanceInfo, OrderDir, PageRef,
    ResolvedWikilink, SkillInfo, Visibility,
};
pub use schema::Migrator;
pub use search::{Granularity, SearchHit};
pub use validate::{Issue, Severity};
