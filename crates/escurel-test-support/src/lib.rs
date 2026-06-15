//! Test-process façade for downstream applications that build on
//! top of Escurel.
//!
//! See [`docs/spec/dx.md`](../../docs/spec/dx.md) §"Test-process
//! façade" for the committed surface and §"Auth in tests" for the
//! commitment that a test using [`AuthMode::TestIssuer`] never
//! needs to import `wiremock`, `jsonwebtoken`, or `rsa` directly.
//!
//! The crate is a `dev-dependency`-shaped façade: it pulls in
//! `escurel-server` (which brings DuckDB and the full indexer),
//! `wiremock`, `jsonwebtoken`, and `rsa` as internal deps so the
//! caller doesn't have to. Downstream applications add this crate
//! under `[dev-dependencies]` only — production binaries pull
//! `escurel-client` instead.
//!
//! # Example
//!
//! ```no_run
//! use escurel_test_support::{
//!     AuthMode, EscurelProcess, FixtureBuilder, Opts, Role,
//! };
//! use escurel_client::ListSkillsRequest;
//!
//! # async fn run() {
//! let process = EscurelProcess::spawn(Opts {
//!     auth: AuthMode::TestIssuer,
//!     fixtures: Some(
//!         FixtureBuilder::new()
//!             .tenant("acme")
//!                 .skill("customer", "---\ntype: skill\nid: customer\n---\n# customer\n")
//!             .done(),
//!     ),
//!     ..Default::default()
//! }).await;
//!
//! let _token = process.mint_token("acme", Role::Agent);
//! let client = process.client();
//! let skills = client.list_skills(ListSkillsRequest::default()).await.unwrap();
//! assert!(skills.skills.iter().any(|s| s.id == "customer"));
//!
//! process.shutdown().await;
//! # }
//! ```

mod auth;
mod fixtures;
mod mcp_client;
mod process;

pub use auth::{AuthMode, Role};
pub use fixtures::{FixtureBuilder, MarkdownBody, TenantFixture};
pub use mcp_client::{McpError, McpTestClient};
pub use process::{ConfigOverrides, EscurelProcess, Opts};
// Re-export so consumers can set `ConfigOverrides.write_acl` in their own
// integration tests without depending on `escurel-server` directly.
pub use escurel_server::WriteAclMode;

// Re-export the request/response types the test author needs so
// they can mirror the `Client` surface without a second
// `use escurel_client::...` line. Keeping the set in sync with
// `escurel-client`'s re-export list is part of the support
// crate's surface contract.
pub use escurel_client::{
    Edge, ExpandBlock, ExpandRequest, ExpandResponse, InstanceInfo, ListInstancesRequest,
    ListInstancesResponse, ListSkillsRequest, ListSkillsResponse, NeighboursRequest,
    NeighboursResponse, PageRef, ResolveRequest, ResolveResponse, RunStoredQueryRequest,
    RunStoredQueryResponse, SearchHit, SearchRequest, SearchResponse, Skill, StoredQueryColumn,
    UpdatePageRequest, UpdatePageResponse, ValidationIssue, WikilinkParsed,
};
