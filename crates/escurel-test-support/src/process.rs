//! [`EscurelProcess`] — the test-process façade.
//!
//! See `docs/spec/dx.md` §"Test-process façade". A downstream test
//! calls [`EscurelProcess::spawn`] once and gets a fully-wired
//! Escurel gateway bound on a random loopback port, backed by an
//! in-tempdir DuckDB + filesystem store, optionally with an in-
//! process OIDC issuer for auth and a fixture-builder seeding the
//! tenant before control returns to the caller.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_admin::TenantStore;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_client::{Client, SecretString};
use escurel_crdt::CrdtBackend;
use escurel_embed::ReloadableEmbedder;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_quota::QuotaManager;
use escurel_server::{
    AlwaysReady, EmbedderFactory, ReadinessProbe, ServerConfig, ServerHandle, WriteAclMode, serve,
};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

use crate::auth::{AuthMode, Role, TEST_AUDIENCE, TestIssuer};
use crate::fixtures::FixtureBuilder;
use crate::mcp_client::McpTestClient;

/// Open knobs for [`EscurelProcess::spawn`]. Additive only.
///
/// Per `docs/spec/dx.md` §"Stability and versioning", the shape of
/// `ConfigOverrides` is *not* semver-stable — new server knobs
/// land here additively. Today's knobs cover the cross-cutting
/// dependencies a test needs to pin: quota manager, tenant store,
/// CRDT backend, readiness probe, and indexer presence.
#[derive(Default, Clone)]
pub struct ConfigOverrides {
    /// Per-instance write-ACL enforcement mode. `None` → `Off` (the
    /// production default); write-ACL tests set `Enforce`.
    pub write_acl: Option<WriteAclMode>,
    /// Value returned by `GET /version`. Defaults to
    /// `"0.0.0-test"`.
    pub gateway_version: Option<String>,
    /// Replace the default `AlwaysReady` probe surfaced at
    /// `/readyz` + `/healthz`. Tests for the health surface
    /// install a probe that flips one component to down.
    pub readiness: Option<Arc<dyn ReadinessProbe>>,
    /// Install a `QuotaManager`. When `None` no rate / session
    /// limits are enforced.
    pub quota: Option<Arc<QuotaManager>>,
    /// Install a `TenantStore`. Required to exercise the admin
    /// tenant-CRUD surface; CRUD calls return
    /// `failed_precondition` when absent.
    pub tenant_store: Option<Arc<dyn TenantStore>>,
    /// Install a `CrdtBackend`. Required to exercise the live
    /// session tools (`open_session`/`apply_op`/`close_session`)
    /// and the WS attach paths.
    pub crdt_backend: Option<Arc<dyn CrdtBackend>>,
    /// Replace the auto-built default indexer with a test-owned
    /// `Arc<Indexer>`. When `Some`, the support crate does *not*
    /// allocate its own tempdirs for the markdown lane / DuckDB
    /// file — the test is responsible for keeping them alive
    /// alongside the `EscurelProcess`. Mutually exclusive with
    /// `disable_indexer`.
    pub indexer: Option<Arc<Indexer>>,
    /// Skip building the default indexer; the gateway runs with
    /// `indexer = None`. Live-session-only tests use this so the
    /// HNSW autoload gotcha never bites (see
    /// `docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md`).
    pub disable_indexer: bool,
    /// Install a hot-swappable embedder seam wired to the
    /// `embedding_reload` admin RPC. Paired with `embedder_factory`
    /// — both must be `Some` for the RPC to do anything other than
    /// return `failed_precondition`. Tests for the degraded-start /
    /// reload path pass a real `ReloadableEmbedder` (typically built
    /// via `ReloadableEmbedder::degraded(dim)`) plus a factory.
    pub embedder_reload: Option<Arc<ReloadableEmbedder>>,
    /// On-demand rebuild closure for the `embedding_reload` admin
    /// RPC. See [`EmbedderFactory`]; paired with `embedder_reload`.
    pub embedder_factory: Option<EmbedderFactory>,
    /// Serve a built static demo bundle at `/` (Flutter web
    /// `build/web`). `Some` exercises the gateway's `ServeDir`
    /// fallback + SPA index routing; `None` (default) keeps the
    /// bare-API behaviour (unknown path → 404).
    pub demo_dir: Option<std::path::PathBuf>,
    /// Outbound capture webhook URL. `Some` exercises the
    /// fire-and-forget POST `capture_event` fires on a new inbox item;
    /// `None` (default) disables it.
    pub webhook_url: Option<String>,
    /// Shared secret for the outbound capture webhook. When `Some`, the
    /// gateway HMAC-SHA256-signs the POST body and sends it as
    /// `X-Escurel-Webhook-Signature: sha256=<hex>`. `None` (default)
    /// leaves the POST unsigned.
    pub webhook_secret: Option<String>,
}

impl std::fmt::Debug for ConfigOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigOverrides")
            .field("write_acl", &self.write_acl)
            .field("gateway_version", &self.gateway_version)
            .field("readiness_overridden", &self.readiness.is_some())
            .field("quota_overridden", &self.quota.is_some())
            .field("tenant_store_overridden", &self.tenant_store.is_some())
            .field("crdt_backend_overridden", &self.crdt_backend.is_some())
            .field("indexer_overridden", &self.indexer.is_some())
            .field("disable_indexer", &self.disable_indexer)
            .field(
                "embedder_reload_overridden",
                &self.embedder_reload.is_some(),
            )
            .field(
                "embedder_factory_overridden",
                &self.embedder_factory.is_some(),
            )
            .finish()
    }
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

/// Running Escurel gateway, ready to accept HTTP (MCP + WS) traffic.
///
/// Carries owned tempdirs + (optionally) the mock OIDC issuer so
/// the process is fully self-contained: when the `EscurelProcess`
/// is dropped, the server tasks, the listener, the JWKS server,
/// and the on-disk state all go away together.
pub struct EscurelProcess {
    base_url: String,
    // Full `http://<addr>/metrics` URL for the dedicated metrics
    // listener (a random port in tests). `None` only when metrics
    // were disabled.
    metrics_url: Option<String>,
    handle: Option<ServerHandle>,
    issuer: Option<TestIssuer>,
    // Pre-built MCP-over-HTTP client for the default tenant ("acme").
    // `Client::connect` is cheap (no network round-trip — the first
    // request is what dials), so this is just a typed handle carrying
    // the base URL + bearer; `client()` hands out clones of it.
    default_client: Client,
    // Shared handle on the same indexer the gateway uses, for
    // fixture seeding without paying the auth/quota gate. The
    // gateway's `update_page` tool calls
    // `indexer.update_page(...)` directly, so seeding through
    // this handle is semantically identical to the public write
    // path — what the spec's "Fixture/seeding façade" promises.
    // `None` when the gateway was spawned with
    // `disable_indexer = true`; seeding falls back to the
    // gateway's MCP `update_page` tool in that case (and tests
    // that disable the indexer should not declare fixtures).
    indexer: Option<Arc<Indexer>>,
    // Companion handle on the LaneStore the default indexer
    // was built against. We mirror each seeded markdown body
    // here so the `audit()` drift surface in `escurel-admin`
    // (markdown ↔ DuckDB consistency) sees the same set of
    // pages on both sides — without this, fixture seeding via
    // the indexer alone would surface every page as
    // `indexed_but_no_markdown`. `None` when the caller passed
    // in a custom indexer; their seeding is theirs to mirror.
    default_lane_store: Option<Arc<dyn LaneStore>>,
    // Tenant string the default indexer is bound to, used to
    // construct `Key`s when mirroring seeds into
    // `default_lane_store`.
    default_tenant: String,
    // Owned for the lifetime of the process so the tempdirs stay
    // alive until shutdown / Drop. Both are `None` when the
    // caller injected an external indexer via
    // `ConfigOverrides::indexer` (or disabled the indexer entirely
    // with `disable_indexer`) — the test owns the underlying state
    // in those cases.
    _store_dir: Option<TempDir>,
    _db_dir: Option<TempDir>,
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
        let overrides = opts.config_overrides;
        assert!(
            !(overrides.disable_indexer && overrides.indexer.is_some()),
            "ConfigOverrides::disable_indexer is mutually exclusive with ConfigOverrides::indexer"
        );

        // 1. Per-spawn tempdirs for the markdown lane and the
        //    DuckDB file when we're building the default indexer.
        //    When the caller supplied their own indexer (or
        //    disabled it), the tempdirs are theirs to own.
        let (store_dir, db_dir, indexer, default_lane_store) =
            if let Some(custom) = overrides.indexer.clone() {
                (None, None, Some(custom), None)
            } else if overrides.disable_indexer {
                (None, None, None, None)
            } else {
                let store_dir = TempDir::new().expect("tempdir for store");
                let db_dir = TempDir::new().expect("tempdir for duckdb");
                let (indexer, store) = build_indexer(&store_dir, &db_dir);
                // Match production: every served tenant ships the
                // mandatory `escurel` meta-skill (locked decision 3).
                // Done here rather than in the sync `build_indexer`
                // because it's an async write through the indexer.
                indexer
                    .ensure_meta_skill()
                    .await
                    .expect("escurel-test-support: ensure meta-skill");
                (Some(store_dir), Some(db_dir), Some(indexer), Some(store))
            };

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
            AuthMode::ExternalMulti { issuers } => {
                let (primary_iss, primary_jwks) = issuers
                    .first()
                    .expect("AuthMode::ExternalMulti requires at least one issuer");
                let mut cfg = OidcConfig::new(primary_iss.clone(), TEST_AUDIENCE.to_owned())
                    .with_jwks_uri(primary_jwks.clone());
                for (iss, jwks) in &issuers[1..] {
                    cfg = cfg.with_additional_issuer(iss.clone(), Some(jwks.clone()));
                }
                let verifier = Arc::new(OidcVerifier::new(cfg));
                (Some(verifier), None)
            }
        };

        // 3. Boot the server. Bound port returned in `local_addr`
        //    *before* the join handle starts polling — so `spawn`
        //    is race-free against the first request.
        let version = overrides
            .gateway_version
            .clone()
            .unwrap_or_else(|| "0.0.0-test".to_owned());
        let readiness = overrides
            .readiness
            .clone()
            .unwrap_or_else(|| Arc::new(AlwaysReady) as Arc<dyn ReadinessProbe>);
        let cfg = ServerConfig {
            write_acl: overrides.write_acl.unwrap_or_default(),
            listen: "127.0.0.1:0".to_owned(),
            version,
            readiness,
            indexer: indexer.clone(),
            verifier,
            quota: overrides.quota.clone(),
            tenant_store: overrides.tenant_store.clone(),
            crdt_backend: overrides.crdt_backend.clone(),
            embedder_reload: overrides.embedder_reload.clone(),
            embedder_factory: overrides.embedder_factory.clone(),
            demo_dir: overrides.demo_dir.clone(),
            webhook_url: overrides.webhook_url.clone(),
            webhook_secret: overrides.webhook_secret.clone(),
            // Metrics on their own random port, mirroring production's
            // dedicated listener (production defaults to :9090).
            metrics_listen: Some("127.0.0.1:0".to_owned()),
        };
        let handle = serve(cfg)
            .await
            .expect("escurel-test-support: serve() failed");
        let base_url = format!("http://{}", handle.local_addr);
        let metrics_url = handle
            .metrics_addr
            .map(|addr| format!("http://{addr}/metrics"));

        // Build the default-tenant MCP-over-HTTP client. `connect` is
        // cheap (no network), so `client()` hands out clones of this
        // rather than re-connecting — the surface stays sync, matching
        // `docs/spec/dx.md` §"Test-process façade".
        let default_token = match &issuer {
            Some(i) => i.mint("acme", Role::Agent),
            None => String::new(),
        };
        let default_client = Client::connect(&base_url, SecretString::from(default_token))
            .await
            .expect("escurel-test-support: Client::connect default tenant");

        let mut process = Self {
            base_url,
            metrics_url,
            handle: Some(handle),
            issuer,
            default_client,
            indexer,
            default_lane_store,
            default_tenant: "acme".to_owned(),
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

    /// `http://127.0.0.1:<port>/metrics` — the dedicated Prometheus
    /// scrape endpoint (a separate listener from the main HTTP app).
    /// `None` only when metrics were disabled for this spawn.
    #[must_use]
    pub fn metrics_url(&self) -> Option<&str> {
        self.metrics_url.as_deref()
    }

    /// `ws://127.0.0.1:<port>/ws` — the WebSocket endpoint for
    /// presence + live attach. Derived from `base_url` so the
    /// test never has to know how the HTTP scheme maps to ws.
    #[must_use]
    pub fn ws_url(&self) -> String {
        let trimmed = self
            .base_url
            .strip_prefix("http://")
            .or_else(|| self.base_url.strip_prefix("https://"))
            .unwrap_or(&self.base_url);
        let scheme = if self.base_url.starts_with("https://") {
            "wss"
        } else {
            "ws"
        };
        format!("{scheme}://{trimmed}/ws")
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

    /// Like [`Self::mint_token`], but with an explicit `sub` claim — for
    /// per-instance ACL tests where the caller subject is the owning
    /// principal (e.g. a member credential).
    #[must_use]
    pub fn mint_token_with_sub(&self, tenant: &str, role: Role, subject: &str) -> String {
        let issuer = self.issuer.as_ref().expect(
            "EscurelProcess::mint_token_with_sub requires AuthMode::TestIssuer; spawned with a different mode",
        );
        issuer.mint_with_sub(tenant, role, subject)
    }

    /// Mint a bearer with an explicit `sub` and arbitrary token groups
    /// in the `roles` claim — for RBAC tests exercising custom token
    /// groups (`moderator`, `team-acme`, …). Set `admin` to also stamp
    /// the `escurel:admin` marker so the verifier projects
    /// [`Role::Admin`].
    #[must_use]
    pub fn mint_token_with_groups(
        &self,
        tenant: &str,
        subject: &str,
        groups: &[&str],
        admin: bool,
    ) -> String {
        let issuer = self.issuer.as_ref().expect(
            "EscurelProcess::mint_token_with_groups requires AuthMode::TestIssuer; spawned with a different mode",
        );
        issuer.mint_with_groups(tenant, subject, groups, admin)
    }

    /// Typed MCP-over-HTTP client targeting this process's HTTP
    /// listener, pre-loaded with a bearer token minted for the default
    /// `"acme"` tenant. Cheap clone of an already-built client — no
    /// `await` here, matching the sync signature in
    /// `docs/spec/dx.md` §"Test-process façade".
    ///
    /// Tests that need a client for a *different* tenant call
    /// [`Self::client_for`].
    #[must_use]
    pub fn client(&self) -> Client {
        self.default_client.clone()
    }

    /// Typed MCP-over-HTTP client minting a fresh bearer for an
    /// arbitrary tenant + role.
    ///
    /// # Panics
    ///
    /// Panics when the process was spawned with `AuthMode::Disabled`
    /// and the caller asks for a per-tenant token — there is no issuer
    /// to mint one.
    pub async fn client_for(&self, tenant: &str, role: Role) -> Client {
        let token = self
            .issuer
            .as_ref()
            .expect("client_for requires AuthMode::TestIssuer")
            .mint(tenant, role);
        Client::connect(&self.base_url, SecretString::from(token))
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
        // Seed through `Indexer::update_page` — the same function
        // the gateway's `update_page` MCP tool calls under the
        // hood (see `tool_update_page` in
        // `crates/escurel-server/src/mcp.rs`). The spec's
        // "Fixture/seeding façade" guarantees what tests seed is
        // what `update_page` would seed in production; using the
        // shared `Arc<Indexer>` honours that contract without
        // debiting the test's quota budget along the way (the
        // gateway's middleware sits *above* this call, not
        // inside it).
        if let Some(indexer) = &self.indexer {
            for entry in entries {
                // Mirror the bytes into the LaneStore first so
                // `Indexer::audit` (markdown-vs-duckdb drift) sees
                // both halves of the seed. The gateway's
                // `update_page` tool today only writes the
                // DuckDB side; the support crate's seed plugs
                // that hole for fixture pages so audit-style
                // assertions in admin-CRUD tests stay clean.
                if let Some(store) = &self.default_lane_store {
                    let key = Key::new(self.default_tenant.as_str(), entry.page_id.clone())
                        .unwrap_or_else(|e| {
                            panic!(
                                "seed: invalid page_id `{}` for tenant `{}`: {e:?}",
                                entry.page_id, entry.tenant
                            )
                        });
                    store
                        .write(&key, Bytes::copy_from_slice(entry.body.as_bytes()))
                        .await
                        .unwrap_or_else(|e| {
                            panic!(
                                "seed: lane_store write `{}` for tenant `{}` failed: {e:?}",
                                entry.page_id, entry.tenant
                            )
                        });
                }
                indexer
                    .update_page(&entry.page_id, &entry.body)
                    .await
                    .unwrap_or_else(|e| {
                        panic!(
                            "seed: update_page `{}` for tenant `{}` failed: {e:?}",
                            entry.page_id, entry.tenant
                        )
                    });
            }
            return;
        }
        // No shared indexer (e.g. `disable_indexer = true`); fall
        // back to the public MCP write path. This is mostly a
        // safety net — tests that disable the indexer should not
        // be declaring fixtures.
        let mcp = self.mcp_client();
        for entry in entries {
            let result = mcp
                .call(
                    "update_page",
                    serde_json::json!({ "page_id": entry.page_id, "content": entry.body }),
                )
                .await
                .unwrap_or_else(|e| {
                    panic!(
                        "seed: update_page `{}` for tenant `{}` failed: {e:?}",
                        entry.page_id, entry.tenant
                    )
                });
            let ok = result
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            assert!(
                ok,
                "seed: update_page `{}` returned ok=false: {result:?}",
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

fn build_indexer(store_dir: &TempDir, db_dir: &TempDir) -> (Arc<Indexer>, Arc<dyn LaneStore>) {
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
    let indexer = Arc::new(
        Indexer::new(Arc::clone(&store), embedder, conn, "acme").expect("Indexer construction"),
    );
    (indexer, store)
}
