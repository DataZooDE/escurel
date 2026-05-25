//! [`EscurelProcess`] — the test-process façade.
//!
//! See `docs/spec/dx.md` §"Test-process façade". A downstream test
//! calls [`EscurelProcess::spawn`] once and gets a fully-wired
//! Escurel gateway bound on a random loopback port, backed by an
//! in-tempdir DuckDB + filesystem store, optionally with an in-
//! process OIDC issuer for auth and a fixture-builder seeding the
//! tenant before control returns to the caller.

use std::sync::Arc;

use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_client::{Client, SecretString, UpdatePageRequest};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::{AlwaysReady, ServerConfig, ServerHandle, serve};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

use crate::auth::{AuthMode, Role, TEST_AUDIENCE, TestIssuer};
use crate::fixtures::FixtureBuilder;
use crate::mcp_client::McpTestClient;

/// Open knobs for [`EscurelProcess::spawn`]. Additive only.
///
/// Today: the `gateway_version` string surfaced at `/version`. The
/// shape is left here so future server knobs (env-var overrides,
/// non-default embedder, custom readiness probe) can land without
/// breaking call sites.
#[derive(Debug, Default, Clone)]
pub struct ConfigOverrides {
    /// Value returned by `GET /version`. Defaults to
    /// `"0.0.0-test"`.
    pub gateway_version: Option<String>,
}

/// Top-level options passed to [`EscurelProcess::spawn`]. `Default`
/// is a no-auth, no-fixtures, default-version gateway — useful for
/// smoke tests that only exercise the dispatcher.
#[derive(Default)]
pub struct Opts {
    pub auth: AuthMode,
    pub fixtures: Option<FixtureBuilder>,
    pub config_overrides: ConfigOverrides,
}

/// Running Escurel gateway, ready to accept HTTP + gRPC traffic.
///
/// Carries owned tempdirs + (optionally) the mock OIDC issuer so
/// the process is fully self-contained: when the `EscurelProcess`
/// is dropped, the server tasks, the listener, the JWKS server,
/// and the on-disk state all go away together.
pub struct EscurelProcess {
    base_url: String,
    grpc_endpoint: String,
    handle: Option<ServerHandle>,
    issuer: Option<TestIssuer>,
    // Pre-connected gRPC client for the default tenant ("acme").
    // We connect once at spawn time so `client()` can stay sync
    // (as the dx.md spec mandates) without needing
    // `block_in_place`, which requires the multi-threaded runtime
    // and would force every downstream test to spell
    // `#[tokio::test(flavor = "multi_thread")]`.
    default_client: Client,
    // Owned for the lifetime of the process so the tempdirs stay
    // alive until shutdown / Drop.
    _store_dir: TempDir,
    _db_dir: TempDir,
}

impl std::fmt::Debug for EscurelProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EscurelProcess")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl EscurelProcess {
    /// Bind a fresh gateway and (optionally) replay `opts.fixtures`
    /// through the public write path before returning.
    ///
    /// Invariants per `docs/spec/dx.md` §"Test-process façade":
    ///
    /// 1. The listener is bound when `spawn` returns — there is no
    ///    race between `spawn` resolving and the first request
    ///    succeeding.
    /// 2. Each call gets its own port + tempdirs, so concurrent
    ///    `cargo test` workers don't collide.
    /// 3. `AuthMode::TestIssuer` stands up an ephemeral JWKS
    ///    server (wiremock under the hood) and an RSA keypair
    ///    that this `EscurelProcess` owns; signing material does
    ///    not leak across spawns.
    pub async fn spawn(opts: Opts) -> Self {
        // 1. Per-spawn tempdirs for the markdown lane and the
        //    DuckDB file. Bound to the EscurelProcess so they
        //    outlive every request but are torn down on Drop.
        let store_dir = TempDir::new().expect("tempdir for store");
        let db_dir = TempDir::new().expect("tempdir for duckdb");

        // 2. Resolve the auth mode. TestIssuer brings up wiremock
        //    + an RSA keypair and threads them into an
        //    `OidcVerifier`. External points the verifier at the
        //    caller's URLs. Disabled leaves `verifier = None`.
        let (verifier, issuer) = match &opts.auth {
            AuthMode::Disabled => (None, None),
            AuthMode::TestIssuer => {
                let issuer = TestIssuer::start().await;
                let cfg = OidcConfig::new(issuer.issuer_url.clone(), TEST_AUDIENCE.to_owned())
                    .with_jwks_uri(issuer.jwks_url.clone());
                let verifier = Arc::new(OidcVerifier::new(cfg));
                (Some(verifier), Some(issuer))
            }
            AuthMode::External {
                issuer_url,
                jwks_url,
            } => {
                let cfg = OidcConfig::new(issuer_url.clone(), TEST_AUDIENCE.to_owned())
                    .with_jwks_uri(jwks_url.clone());
                let verifier = Arc::new(OidcVerifier::new(cfg));
                (Some(verifier), None)
            }
        };

        // 3. Bring up the indexer against the per-spawn tempdirs.
        //    The fixtures replay below uses the same write path
        //    `update_page` does in production; here we still need
        //    a real Indexer wired into the gateway's `ServerConfig`.
        let indexer = build_indexer(&store_dir, &db_dir);

        // 4. Boot the server. Bound port returned in `local_addr`
        //    *before* the join handle starts polling — so `spawn`
        //    is race-free against the first request.
        let version = opts
            .config_overrides
            .gateway_version
            .unwrap_or_else(|| "0.0.0-test".to_owned());
        let cfg = ServerConfig {
            listen: "127.0.0.1:0".to_owned(),
            grpc_listen: Some("127.0.0.1:0".to_owned()),
            version,
            readiness: Arc::new(AlwaysReady),
            indexer: Some(Arc::clone(&indexer)),
            verifier,
            quota: None,
            tenant_store: None,
            crdt_backend: None,
        };
        let handle = serve(cfg)
            .await
            .expect("escurel-test-support: serve() failed");
        let base_url = format!("http://{}", handle.local_addr);
        let grpc_endpoint = format!(
            "http://{}",
            handle.grpc_addr.expect("grpc listener bound by spawn()")
        );

        // Connect the default-tenant client once. Sync `client()`
        // calls hand out clones of this rather than re-connecting,
        // so the surface stays sync on the single-thread runtime
        // most `#[tokio::test]`s use.
        let default_token = match &issuer {
            Some(i) => i.mint("acme", Role::Agent),
            None => "test-disabled".to_owned(),
        };
        let default_client = Client::connect(&grpc_endpoint, SecretString::from(default_token))
            .await
            .expect("escurel-test-support: Client::connect default tenant");

        let mut process = Self {
            base_url,
            grpc_endpoint,
            handle: Some(handle),
            issuer,
            default_client,
            _store_dir: store_dir,
            _db_dir: db_dir,
        };

        // 5. Replay fixtures through the public write path. Per
        //    `docs/spec/dx.md` §"Fixture/seeding façade", a
        //    fixture entry is exactly an `update_page` call — no
        //    side-door into the indexer.
        if let Some(builder) = opts.fixtures {
            process.seed(builder).await;
        }

        process
    }

    /// `http://127.0.0.1:<port>` — the HTTP base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `<base_url>/mcp` — the MCP-over-HTTP endpoint.
    #[must_use]
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }

    /// `http://127.0.0.1:<port>` — the gRPC endpoint, ready to be
    /// passed to [`escurel_client::Client::connect`]. The contract
    /// in `docs/spec/dx.md` §"Test-process façade" promises the
    /// listener is bound when [`Self::spawn`] returns, so this URL
    /// is always dial-able.
    #[must_use]
    pub fn grpc_endpoint(&self) -> &str {
        &self.grpc_endpoint
    }

    /// Mint a fresh bearer token for `tenant` with `role`. Only
    /// valid when [`AuthMode::TestIssuer`] is selected — other
    /// modes panic, because the caller has no business asking the
    /// support crate to sign tokens for a real OIDC realm.
    ///
    /// # Panics
    ///
    /// Panics when the running process was spawned with
    /// `AuthMode::Disabled` or `AuthMode::External`. Tests that
    /// need bearer tokens against an external OIDC must mint them
    /// out-of-band.
    #[must_use]
    pub fn mint_token(&self, tenant: &str, role: Role) -> String {
        let issuer = self.issuer.as_ref().expect(
            "EscurelProcess::mint_token requires AuthMode::TestIssuer; spawned with a different mode",
        );
        issuer.mint(tenant, role)
    }

    /// Typed gRPC client targeting this process's gRPC listener,
    /// pre-loaded with a bearer token minted for the default
    /// `"acme"` tenant. Cheap clone of an already-connected
    /// channel — no `await` here, matching the sync signature in
    /// `docs/spec/dx.md` §"Test-process façade".
    ///
    /// Tests that need a client for a *different* tenant call
    /// [`Self::client_for`].
    #[must_use]
    pub fn client(&self) -> Client {
        self.default_client.clone()
    }

    /// Typed gRPC client minting a fresh bearer for an arbitrary
    /// tenant + role. This *is* async because it has to open a
    /// new tonic channel — the sync `client()` path covers the
    /// common case.
    ///
    /// # Panics
    ///
    /// Panics when the process was spawned with
    /// `AuthMode::Disabled` and the caller asks for a per-tenant
    /// token — there is no issuer to mint one.
    pub async fn client_for(&self, tenant: &str, role: Role) -> Client {
        let token = self
            .issuer
            .as_ref()
            .expect("client_for requires AuthMode::TestIssuer")
            .mint(tenant, role);
        Client::connect(&self.grpc_endpoint, SecretString::from(token))
            .await
            .expect("Client::connect")
    }

    /// Typed MCP-over-HTTP client targeting `POST /mcp`. Pre-
    /// loaded with a bearer token for the default `"acme"` tenant
    /// when auth is enabled; for `AuthMode::Disabled` no
    /// `Authorization` header is sent.
    #[must_use]
    pub fn mcp_client(&self) -> McpTestClient {
        let bearer = self.issuer.as_ref().map(|i| i.mint("acme", Role::Agent));
        McpTestClient::new(self.mcp_url(), bearer)
    }

    /// Signal graceful shutdown and await both server tasks.
    /// Equivalent to dropping the `EscurelProcess`, but explicit —
    /// tests that need to assert "the port is freed" call this
    /// rather than relying on `Drop` ordering.
    pub async fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await;
        }
    }

    /// Replay every entry in `builder` through the gateway's
    /// `update_page` MCP tool. Per the spec's "Fixture/seeding
    /// façade" §, seeding never bypasses the public write path —
    /// what tests seed is what `update_page` would seed in
    /// production.
    ///
    /// Today the underlying `Indexer` is single-tenant: it was
    /// constructed at `spawn` time with the literal `"acme"`
    /// tenant string baked in. Fixtures declared under a different
    /// `tenant(...)` name still seed (the gateway routes them to
    /// the same Indexer); when M3-grade per-tenant indexers
    /// arrive, this method gains a per-tenant client without
    /// changing the public surface.
    ///
    /// # Panics
    ///
    /// Panics on transport / JSON-RPC failure or a non-`ok`
    /// validation response. Seeding errors in a test fixture
    /// would be far more surprising than a panic.
    async fn seed(&mut self, builder: FixtureBuilder) {
        let entries = builder.into_entries();
        if entries.is_empty() {
            return;
        }
        for entry in entries {
            // Use the pre-connected default client — the gateway's
            // single Indexer ignores the tenant claim today, so a
            // single client suffices. The `entry.tenant` is still
            // honoured for `mint_token(tenant, role)` callers.
            let resp = self
                .default_client
                .update_page(UpdatePageRequest {
                    page_id: entry.page_id.clone(),
                    content: entry.body,
                })
                .await
                .unwrap_or_else(|e| {
                    panic!(
                        "seed: update_page `{}` for tenant `{}` failed: {e:?}",
                        entry.page_id, entry.tenant
                    )
                });
            assert!(
                resp.ok,
                "seed: update_page `{}` returned ok=false: {resp:?}",
                entry.page_id
            );
        }
    }
}

impl Drop for EscurelProcess {
    fn drop(&mut self) {
        // Best-effort: if `shutdown()` wasn't called explicitly,
        // signal the server tasks anyway. The drop runs on whatever
        // runtime context the caller is in — usually a tokio
        // worker. We *don't* `block_on(handle.shutdown())` because
        // dropping from within an async context would risk a
        // runtime-within-runtime panic; instead we send the
        // shutdown signal and let the spawned tasks finish on the
        // runtime's own schedule.
        if let Some(handle) = self.handle.take() {
            // Use the explicit `shutdown` path on a spawned task
            // when a runtime is reachable; otherwise just drop the
            // handle (the channels close and the tasks unwind).
            if tokio::runtime::Handle::try_current().is_ok() {
                let h = tokio::spawn(async move {
                    handle.shutdown().await;
                });
                // We don't await the JoinHandle here — Drop must
                // not block — but the spawn ensures the server
                // tasks see the shutdown signal promptly.
                drop(h);
            } else {
                drop(handle);
            }
        }
    }
}

fn build_indexer(store_dir: &TempDir, db_dir: &TempDir) -> Arc<Indexer> {
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn =
        Connection::open(db_dir.path().join("escurel.duckdb")).expect("open per-spawn duckdb");
    Migrator::up(&conn).expect("duckdb migrations");
    // The Indexer is single-tenant today; the support crate seeds
    // every fixture under the default "acme" tenant on its disk
    // layout, but the in-memory Indexer doesn't enforce the
    // tenant string — the verified token threads that through at
    // request time. Bind it to "acme" so the on-disk layout
    // matches the tenant string the test mints tokens for.
    Arc::new(
        Indexer::new(Arc::clone(&store), embedder, conn, "acme").expect("Indexer construction"),
    )
}
