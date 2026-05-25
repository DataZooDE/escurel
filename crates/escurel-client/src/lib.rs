//! Typed RPC client for the Escurel v1 surface.
//!
//! This crate is the *typed wrapper* downstream applications import
//! into their backend. It is a leaf crate: it depends on
//! [`escurel-proto`] (the tonic codegen) and `tonic` for the
//! transport, and nothing else from the escurel workspace. In
//! particular it does **not** pull in `escurel-server` — that crate's
//! dependency tree includes DuckDB, candle, and the indexer; none of
//! that has any business in an application's binary.
//!
//! See [`docs/spec/dx.md`](../../docs/spec/dx.md) §"Client crate for
//! the app's backend" for the wire contract.
//!
//! # Transport
//!
//! For M-DX-1 the client speaks **native gRPC** under the hood —
//! that is the natural fit for the tonic codegen we already ship in
//! `escurel-proto`. The spec leaves the default transport open and
//! tilts toward HTTP-MCP eventually for downstream-dependency-
//! footprint reasons; an HTTP transport selected via an `Opts` knob
//! is planned for a follow-up PR. Method signatures are unchanged
//! across that switch — only the wire bytes move.
//!
//! # Example
//!
//! ```no_run
//! use escurel_client::{Client, ListSkillsRequest};
//! use secrecy::SecretString;
//!
//! # async fn run() -> Result<(), escurel_client::Error> {
//! let token = SecretString::from("eyJ…");
//! let client = Client::connect("http://127.0.0.1:8081", token).await?;
//! let skills = client.list_skills(ListSkillsRequest::default()).await?;
//! for s in skills.skills {
//!     println!("{}: {}", s.id, s.description);
//! }
//! # Ok(())
//! # }
//! ```

mod error;

pub use error::Error;

// Re-export the request/response types the downstream caller needs
// so they never pin `escurel-proto` directly. The set tracks the
// signature list in `docs/spec/dx.md` §"Client crate for the app's
// backend" one-to-one.
pub use escurel_proto::v1::{
    Edge, ExpandBlock, ExpandRequest, ExpandResponse, InstanceInfo, ListInstancesRequest,
    ListInstancesResponse, ListSkillsRequest, ListSkillsResponse, NeighboursRequest,
    NeighboursResponse, PageRef, ResolveRequest, ResolveResponse, RunStoredQueryRequest,
    RunStoredQueryResponse, SearchHit, SearchRequest, SearchResponse, Skill, StoredQueryColumn,
    UpdatePageRequest, UpdatePageResponse, ValidationIssue, WikilinkParsed,
};
// Re-exported so callers don't need to depend on `secrecy` directly
// just to spell out a token. Keeping the version in sync with this
// crate's `Cargo.toml` is part of the semver contract.
pub use secrecy::SecretString;

use escurel_proto::v1::escurel_client::EscurelClient;
use secrecy::ExposeSecret as _;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::Channel;

/// Typed gRPC client for the Escurel v1 surface.
///
/// `Client` is opaque on purpose: the underlying tonic channel and
/// the bearer token are private, the only public knobs are the
/// constructor and the per-RPC methods. This keeps the semver
/// surface small.
///
/// The bearer token lives inside a [`secrecy::SecretString`] and is
/// never returned by any accessor, nor included in the type's
/// `Debug` output.
#[derive(Clone)]
pub struct Client {
    inner: EscurelClient<Channel>,
    bearer: MetadataValue<Ascii>,
    // Keep the secret around so it remains in `SecretString`
    // custody for the lifetime of the client; if we only kept the
    // pre-formatted `bearer` we'd silently exit the `SecretString`
    // zeroisation contract on drop.
    _token: SecretString,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately do not print `bearer` or `_token` — they
        // carry the bearer JWT. The crate's invariant is that no
        // tooling that calls `format!("{client:?}")` (logs,
        // panic-traces, `dbg!`) ever leaks the token.
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// Dial the gateway at `endpoint` (e.g. `http://127.0.0.1:8081`)
    /// and authenticate subsequent RPCs with `token` as the bearer.
    ///
    /// Errors:
    /// - [`Error::InvalidEndpoint`] if `endpoint` is not a valid URL.
    /// - [`Error::InvalidToken`] if `token` contains bytes that are
    ///   not legal in an HTTP header value.
    /// - [`Error::Connect`] if the transport handshake fails.
    pub async fn connect(endpoint: &str, token: SecretString) -> Result<Self, Error> {
        let bearer: MetadataValue<Ascii> = format!("Bearer {}", token.expose_secret())
            .parse()
            .map_err(|_| Error::InvalidToken)?;
        let channel = Channel::from_shared(endpoint.to_owned())
            .map_err(|_| Error::InvalidEndpoint(endpoint.to_owned()))?
            .connect()
            .await
            .map_err(Error::Connect)?;
        Ok(Self {
            inner: EscurelClient::new(channel),
            bearer,
            _token: token,
        })
    }

    /// Hybrid vector + FTS search. See `protocol.md` §search.
    pub async fn search(&self, req: SearchRequest) -> Result<SearchResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.search(self.authed(req)).await?.into_inner())
    }

    /// Parse a `[[wikilink]]` and look up its target page.
    pub async fn resolve(&self, req: ResolveRequest) -> Result<ResolveResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.resolve(self.authed(req)).await?.into_inner())
    }

    /// Fetch a page's frontmatter, body, and outbound wikilinks.
    pub async fn expand(&self, req: ExpandRequest) -> Result<ExpandResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.expand(self.authed(req)).await?.into_inner())
    }

    /// Typed link-graph traversal.
    pub async fn neighbours(&self, req: NeighboursRequest) -> Result<NeighboursResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.neighbours(self.authed(req)).await?.into_inner())
    }

    /// Return the tenant's Tier-1 skill catalogue.
    pub async fn list_skills(&self, req: ListSkillsRequest) -> Result<ListSkillsResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.list_skills(self.authed(req)).await?.into_inner())
    }

    /// Enumerate instances of a skill.
    pub async fn list_instances(
        &self,
        req: ListInstancesRequest,
    ) -> Result<ListInstancesResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.list_instances(self.authed(req)).await?.into_inner())
    }

    /// Execute a `[[query::<id>]]` instance with named parameters.
    pub async fn run_stored_query(
        &self,
        req: RunStoredQueryRequest,
    ) -> Result<RunStoredQueryResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client
            .run_stored_query(self.authed(req))
            .await?
            .into_inner())
    }

    /// Upsert a markdown page (the public write path).
    pub async fn update_page(&self, req: UpdatePageRequest) -> Result<UpdatePageResponse, Error> {
        let mut client = self.inner.clone();
        Ok(client.update_page(self.authed(req)).await?.into_inner())
    }

    /// Wrap a request body in a tonic `Request<T>` with the bearer
    /// metadata attached. Cloning a `MetadataValue<Ascii>` is cheap
    /// (it's a bytes-backed handle).
    fn authed<T>(&self, body: T) -> tonic::Request<T> {
        let mut req = tonic::Request::new(body);
        req.metadata_mut()
            .insert("authorization", self.bearer.clone());
        req
    }
}
