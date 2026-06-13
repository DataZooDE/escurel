//! `ESCUREL_*` configuration surface + backend wiring for the
//! deployable `escurel-server` binary.
//!
//! [`EscurelConfig::from_env`] reads the 12-factor config surface
//! (principle 3): a TOML base file at `$ESCUREL_CONFIG` (optional)
//! overlaid by `ESCUREL_*` environment variables. The variable names
//! are the ones the substrate Nomad jobspec pins
//! (`docs/deploy/escurel.nomad.hcl`) and the spec config table
//! (`docs/spec/README.md §Configuration`) — they are the binary's
//! locked config surface.
//!
//! [`EscurelConfig::build`] turns the parsed config into a
//! [`crate::ServerConfig`] plus the live backends it references
//! (LaneStore, embedder, OIDC verifier, quota manager, tenant store,
//! indexer, CRDT backend). The embedder is wrapped in a
//! [`ReloadableEmbedder`] so a *degraded start* (model failed to
//! load) can boot with a placeholder and be hot-reloaded later by the
//! `embedding_reload` admin RPC — see the module docs on
//! [`ReloadableEmbedder`].
//!
//! ## Environment variables
//!
//! | var | default | meaning |
//! |---|---|---|
//! | `ESCUREL_CONFIG` | — | path to a TOML base file; env vars override it |
//! | `VERSION` / `ESCUREL_VERSION` | `0.0.0-dev` | body of `GET /version` |
//! | `ENV` / `ESCUREL_ENV` | `dev` | log field `env` |
//! | `ESCUREL_SERVER_DATA_DIR` | `/data` | host-volume root for DuckDB + FsStore + tenants |
//! | `ESCUREL_SEED_DIR` | — | markdown corpus seeded into the tenant at boot (idempotent), e.g. `examples/crm-demo` |
//! | `ESCUREL_WEBHOOK_URL` | — | outbound capture webhook; fire-and-forget POST of each new `capture_event` (M7) |
//! | `ESCUREL_WEBHOOK_SECRET` | — | shared secret; when set the webhook body is HMAC-SHA256-signed via `X-Escurel-Webhook-Signature: sha256=<hex>` |
//! | `ESCUREL_SERVER_LISTEN_HTTP` | `0.0.0.0:8080` | HTTP listener (MCP/WS/REST) |
//! | `ESCUREL_TENANT` | `default` | single-tenant indexer's tenant id |
//! | `ESCUREL_STORAGE_BACKEND` | `fs` | `fs` or `s3` |
//! | `ESCUREL_STORAGE_S3_BUCKET` | — | S3 bucket (backend=s3) |
//! | `ESCUREL_STORAGE_S3_ENDPOINT` | — | S3 endpoint URL (backend=s3) |
//! | `ESCUREL_STORAGE_S3_PREFIX` | `` | S3 key prefix (backend=s3) |
//! | `ESCUREL_STORAGE_S3_REGION` | `us-east-1` | S3 region label (backend=s3) |
//! | `ESCUREL_STORAGE_S3_PATH_STYLE` | `true` | path-style addressing (informational; the S3 store always uses path-style) |
//! | `ESCUREL_STORAGE_S3_ACCESS_KEY_ID` | — | S3 access key (backend=s3) |
//! | `ESCUREL_STORAGE_S3_SECRET_ACCESS_KEY` | — | S3 secret key (backend=s3) |
//! | `ESCUREL_AUTH_OIDC_ISSUER` | — | OIDC issuer; unset → unauthenticated dev mode |
//! | `ESCUREL_AUTH_OIDC_AUDIENCE` | `escurel` | OIDC audience |
//! | `ESCUREL_AUTH_TENANT_CLAIM` | `tenant` | JWT claim carrying the tenant id |
//! | `ESCUREL_AUTH_ADMIN_ROLE_CLAIM` | `roles` | JWT claim listing roles |
//! | `ESCUREL_AUTH_ADMIN_ROLE_VALUE` | `escurel:admin` | role value granting admin |
//! | `ESCUREL_AUTH_JWKS_REFRESH_SECS` | `300` | JWKS cache TTL (seconds) |
//! | `ESCUREL_AUTH_JWKS_URI` | derived from issuer | explicit JWKS URL (e.g. Triton's `<issuer>/.well-known/jwks.json`) |
//! | `ESCUREL_AUTH_OIDC_ISSUER_2` | — | optional SECOND trusted issuer (e.g. Carl, for the dashboard's self-minted token); shares the audience + tenant claim |
//! | `ESCUREL_AUTH_JWKS_URI_2` | derived from issuer #2 | explicit JWKS URL for the second issuer (e.g. Carl's `<issuer>/jwks.json`) |
//! | `ESCUREL_EMBEDDING_PROVIDER` | `zero` | `zero`, `gemini`, or `embeddinggemma` |
//! | `ESCUREL_EMBEDDING_MODEL` | provider default | model id |
//! | `ESCUREL_EMBEDDING_DEVICE` | `cpu` | candle device (informational; CPU only today) |
//! | `ESCUREL_EMBEDDING_DIM` | `768` | vector dimension |
//! | `ESCUREL_GEMINI_API_KEY` | — | Gemini API key (provider=gemini) |

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use duckdb::Connection;
use escurel_admin::FsTenantStore;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_embed::{Embedder, ReloadableEmbedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_storage::{FsStore, LaneStore};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::config_probe::DependencyProbe;
use crate::{EmbedderFactory, ServerConfig, serve};

/// Default vector dimension (EmbeddingGemma 768).
const DEFAULT_DIM: usize = 768;

/// Storage backend selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    Fs,
    S3,
}

/// Embedding provider selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingProvider {
    /// Zero-vector stub (`ZeroEmbedder`). Dev default.
    Zero,
    /// Gemini HTTP embedder (feature `gemini`).
    Gemini,
    /// Local candle EmbeddingGemma (feature `embeddinggemma`).
    EmbeddingGemma,
}

/// Errors raised while loading config or building backends.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("reading config file {path}: {source}")]
    ReadFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing TOML config {path}: {source}")]
    ParseToml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid value for {var}: {value:?} ({reason})")]
    InvalidValue {
        var: &'static str,
        value: String,
        reason: &'static str,
    },
    #[error("{var} is required when ESCUREL_STORAGE_BACKEND=s3")]
    MissingS3Field { var: &'static str },
    #[error(
        "ESCUREL_EMBEDDING_PROVIDER={provider} requires the `{feature}` cargo feature; \
         this binary was built without it"
    )]
    EmbedderFeatureDisabled {
        provider: &'static str,
        feature: &'static str,
    },
    #[error("{provider} embedder requires {var} to be set")]
    MissingEmbedderField {
        provider: &'static str,
        var: &'static str,
    },
    #[error("creating data dir {path}: {source}")]
    DataDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("opening DuckDB at {path}: {source}")]
    DuckdbOpen {
        path: String,
        #[source]
        source: duckdb::Error,
    },
    #[error("applying DuckDB migrations: {0}")]
    Migrate(#[from] escurel_index::schema::MigrationError),
    #[error("building indexer: {0}")]
    Indexer(#[from] escurel_index::IndexerError),
}

/// TOML base layer. Every field is optional; env vars overlay it.
/// The shape mirrors the spec's example TOML but only the keys the
/// binary actually consumes are modelled.
#[derive(Debug, Default, Deserialize)]
struct TomlConfig {
    #[serde(default)]
    server: TomlServer,
    #[serde(default)]
    auth: TomlAuth,
    #[serde(default)]
    storage: TomlStorage,
    #[serde(default)]
    embedding: TomlEmbedding,
    #[serde(default)]
    observability: TomlObservability,
}

#[derive(Debug, Default, Deserialize)]
struct TomlServer {
    data_dir: Option<String>,
    listen_http: Option<String>,
    tenant: Option<String>,
    version: Option<String>,
    env: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TomlAuth {
    oidc_issuer: Option<String>,
    oidc_audience: Option<String>,
    tenant_claim: Option<String>,
    admin_role_claim: Option<String>,
    admin_role_value: Option<String>,
    jwks_refresh_secs: Option<u64>,
    jwks_uri: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TomlStorage {
    backend: Option<String>,
    s3_bucket: Option<String>,
    s3_endpoint: Option<String>,
    s3_prefix: Option<String>,
    s3_region: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TomlEmbedding {
    provider: Option<String>,
    model: Option<String>,
    device: Option<String>,
    dim: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct TomlObservability {
    metrics_listen: Option<String>,
}

/// Auth config (present only when an OIDC issuer is set).
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub issuer: String,
    pub audience: String,
    pub tenant_claim: String,
    pub admin_role_claim: String,
    pub admin_role_value: String,
    pub jwks_refresh: Duration,
    /// Explicit JWKS URL override. When `None`, the verifier derives one from
    /// the issuer (Keycloak's `<issuer>/protocol/openid-connect/certs`). Set
    /// this for issuers that publish elsewhere — e.g. Triton at
    /// `<issuer>/.well-known/jwks.json`.
    pub jwks_uri: Option<String>,
    /// Additional trusted issuers beyond the primary, each an
    /// `(issuer, jwks_uri_override)` pair. Empty → single-issuer (the
    /// historical behaviour). The substrate sets one entry so a single
    /// Escurel trusts both Triton (forwarded inbound bearer) and Carl
    /// (self-minted dashboard token) — see `ESCUREL_AUTH_OIDC_ISSUER_2`.
    pub additional_issuers: Vec<(String, Option<String>)>,
}

/// S3 storage config (present only when backend == s3).
#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub endpoint: String,
    pub prefix: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// Resolved, validated configuration for the server binary.
#[derive(Debug, Clone)]
pub struct EscurelConfig {
    pub version: String,
    pub env: String,
    pub data_dir: PathBuf,
    pub listen_http: String,
    pub tenant: String,
    pub storage_backend: StorageBackend,
    pub s3: Option<S3Config>,
    pub auth: Option<AuthConfig>,
    pub embedding_provider: EmbeddingProvider,
    pub embedding_model: Option<String>,
    pub embedding_device: String,
    pub embedding_dim: usize,
    pub gemini_api_key: Option<String>,
    /// Optional built demo bundle (Flutter web `build/web`) to serve
    /// at `/`. `None` → no static serving. Set from
    /// `ESCUREL_SERVE_DEMO_DIR`.
    pub demo_dir: Option<PathBuf>,
    /// Optional directory of markdown to seed into the tenant at boot
    /// (e.g. `examples/crm-demo`). `None` → no seeding. Idempotent.
    /// Set from `ESCUREL_SEED_DIR`.
    pub seed_dir: Option<PathBuf>,
    /// Optional outbound capture webhook URL (`ESCUREL_WEBHOOK_URL`).
    /// `Some` → `capture_event` fires a fire-and-forget POST of the new
    /// event; `None` (default) disables it.
    pub webhook_url: Option<String>,
    /// Optional shared secret authenticating the outbound capture
    /// webhook (`ESCUREL_WEBHOOK_SECRET`). When set, the gateway signs
    /// each POST body with HMAC-SHA256 and sends it as
    /// `X-Escurel-Webhook-Signature: sha256=<hex>`; the runner verifies
    /// it against the same secret. `None` (default) leaves the POST
    /// unsigned.
    pub webhook_secret: Option<String>,
    /// Dedicated Prometheus `/metrics` listener
    /// (`ESCUREL_OBSERVABILITY_METRICS_LISTEN`, default
    /// `0.0.0.0:9090`). `None` when explicitly emptied — disables
    /// scraping.
    pub metrics_listen: Option<String>,
}

/// Source of an environment lookup — abstracted so `from_env` is
/// testable with an in-memory map (no `std::env` mutation, which is
/// process-global and races concurrent tests).
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

/// `std::env`-backed source used by the binary.
pub struct OsEnv;

impl EnvSource for OsEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl<F> EnvSource for F
where
    F: Fn(&str) -> Option<String>,
{
    fn get(&self, key: &str) -> Option<String> {
        self(key)
    }
}

impl EscurelConfig {
    /// Load config from the process environment (the binary's entry
    /// point).
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a TOML base file is unreadable /
    /// malformed, a value fails to parse, or a required field for the
    /// selected backend is missing.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(&OsEnv)
    }

    /// Load config from an arbitrary [`EnvSource`]. The TOML base file
    /// (if `ESCUREL_CONFIG` is set in the source) is read from disk;
    /// env values from the source overlay it.
    ///
    /// # Errors
    ///
    /// See [`from_env`](Self::from_env).
    pub fn from_source(env: &dyn EnvSource) -> Result<Self, ConfigError> {
        let toml_cfg = match env.get("ESCUREL_CONFIG") {
            Some(path) if !path.is_empty() => {
                let raw =
                    std::fs::read_to_string(&path).map_err(|source| ConfigError::ReadFile {
                        path: path.clone(),
                        source,
                    })?;
                toml::from_str::<TomlConfig>(&raw)
                    .map_err(|source| ConfigError::ParseToml { path, source })?
            }
            _ => TomlConfig::default(),
        };

        // Helper: env var, then TOML fallback, then literal default.
        let pick = |var: &str, toml_val: Option<String>, default: &str| -> String {
            env.get(var)
                .or(toml_val)
                .unwrap_or_else(|| default.to_owned())
        };

        let version = env
            .get("VERSION")
            .or_else(|| env.get("ESCUREL_VERSION"))
            .or(toml_cfg.server.version)
            .unwrap_or_else(|| "0.0.0-dev".to_owned());
        let server_env = env
            .get("ENV")
            .or_else(|| env.get("ESCUREL_ENV"))
            .or(toml_cfg.server.env)
            .unwrap_or_else(|| "dev".to_owned());

        let data_dir = PathBuf::from(pick(
            "ESCUREL_SERVER_DATA_DIR",
            toml_cfg.server.data_dir,
            "/data",
        ));
        // Optional static demo bundle served at `/`. Empty / unset →
        // no static serving (the gateway stays bare-API).
        let demo_dir = env
            .get("ESCUREL_SERVE_DEMO_DIR")
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        // Optional markdown corpus seeded into the tenant at boot.
        let seed_dir = env
            .get("ESCUREL_SEED_DIR")
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        // Optional outbound capture webhook (fire-and-forget POST).
        let webhook_url = env
            .get("ESCUREL_WEBHOOK_URL")
            .filter(|s| !s.trim().is_empty());
        // Optional shared secret authenticating the capture webhook
        // (HMAC-SHA256 over the POST body). An empty value is treated
        // as unset — an unsigned webhook.
        let webhook_secret = env
            .get("ESCUREL_WEBHOOK_SECRET")
            .filter(|s| !s.trim().is_empty());
        // Dedicated Prometheus `/metrics` listener. Default
        // `0.0.0.0:9090`; an explicitly-empty value disables scraping.
        let metrics_listen_raw = pick(
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN",
            toml_cfg.observability.metrics_listen,
            "0.0.0.0:9090",
        );
        let metrics_listen = if metrics_listen_raw.trim().is_empty() {
            None
        } else {
            Some(metrics_listen_raw)
        };
        let listen_http = pick(
            "ESCUREL_SERVER_LISTEN_HTTP",
            toml_cfg.server.listen_http,
            "0.0.0.0:8080",
        );
        let tenant = pick("ESCUREL_TENANT", toml_cfg.server.tenant, "default");

        // --- storage ---
        let backend_str = pick("ESCUREL_STORAGE_BACKEND", toml_cfg.storage.backend, "fs");
        let storage_backend = match backend_str.as_str() {
            "fs" => StorageBackend::Fs,
            "s3" => StorageBackend::S3,
            other => {
                return Err(ConfigError::InvalidValue {
                    var: "ESCUREL_STORAGE_BACKEND",
                    value: other.to_owned(),
                    reason: "expected `fs` or `s3`",
                });
            }
        };
        let s3 = if storage_backend == StorageBackend::S3 {
            Some(S3Config {
                bucket: require_s3(env, "ESCUREL_STORAGE_S3_BUCKET", toml_cfg.storage.s3_bucket)?,
                endpoint: require_s3(
                    env,
                    "ESCUREL_STORAGE_S3_ENDPOINT",
                    toml_cfg.storage.s3_endpoint,
                )?,
                prefix: env
                    .get("ESCUREL_STORAGE_S3_PREFIX")
                    .or(toml_cfg.storage.s3_prefix)
                    .unwrap_or_default(),
                region: env
                    .get("ESCUREL_STORAGE_S3_REGION")
                    .or(toml_cfg.storage.s3_region)
                    .unwrap_or_else(|| "us-east-1".to_owned()),
                access_key_id: require_s3(env, "ESCUREL_STORAGE_S3_ACCESS_KEY_ID", None)?,
                secret_access_key: require_s3(env, "ESCUREL_STORAGE_S3_SECRET_ACCESS_KEY", None)?,
            })
        } else {
            None
        };

        // --- auth (optional) ---
        let auth = match env
            .get("ESCUREL_AUTH_OIDC_ISSUER")
            .or(toml_cfg.auth.oidc_issuer)
        {
            Some(issuer) if !issuer.is_empty() => {
                let jwks_refresh_secs = match env.get("ESCUREL_AUTH_JWKS_REFRESH_SECS") {
                    Some(raw) => raw.parse::<u64>().map_err(|_| ConfigError::InvalidValue {
                        var: "ESCUREL_AUTH_JWKS_REFRESH_SECS",
                        value: raw,
                        reason: "expected a non-negative integer (seconds)",
                    })?,
                    None => toml_cfg.auth.jwks_refresh_secs.unwrap_or(300),
                };
                Some(AuthConfig {
                    issuer,
                    audience: pick(
                        "ESCUREL_AUTH_OIDC_AUDIENCE",
                        toml_cfg.auth.oidc_audience,
                        "escurel",
                    ),
                    tenant_claim: pick(
                        "ESCUREL_AUTH_TENANT_CLAIM",
                        toml_cfg.auth.tenant_claim,
                        "tenant",
                    ),
                    admin_role_claim: pick(
                        "ESCUREL_AUTH_ADMIN_ROLE_CLAIM",
                        toml_cfg.auth.admin_role_claim,
                        "roles",
                    ),
                    admin_role_value: pick(
                        "ESCUREL_AUTH_ADMIN_ROLE_VALUE",
                        toml_cfg.auth.admin_role_value,
                        "escurel:admin",
                    ),
                    jwks_refresh: Duration::from_secs(jwks_refresh_secs),
                    jwks_uri: env
                        .get("ESCUREL_AUTH_JWKS_URI")
                        .filter(|s| !s.is_empty())
                        .or(toml_cfg.auth.jwks_uri),
                    // Optional second trusted issuer (additive; absent →
                    // single-issuer). Its JWKS URI is explicit when set,
                    // else derived from the issuer.
                    additional_issuers: env
                        .get("ESCUREL_AUTH_OIDC_ISSUER_2")
                        .filter(|s| !s.is_empty())
                        .map(|issuer2| {
                            let jwks2 =
                                env.get("ESCUREL_AUTH_JWKS_URI_2").filter(|s| !s.is_empty());
                            vec![(issuer2, jwks2)]
                        })
                        .unwrap_or_default(),
                })
            }
            _ => None,
        };

        // --- embedding ---
        let provider_str = pick(
            "ESCUREL_EMBEDDING_PROVIDER",
            toml_cfg.embedding.provider,
            "zero",
        );
        let embedding_provider = match provider_str.as_str() {
            "zero" => EmbeddingProvider::Zero,
            "gemini" => EmbeddingProvider::Gemini,
            "embeddinggemma" => EmbeddingProvider::EmbeddingGemma,
            other => {
                return Err(ConfigError::InvalidValue {
                    var: "ESCUREL_EMBEDDING_PROVIDER",
                    value: other.to_owned(),
                    reason: "expected `zero`, `gemini`, or `embeddinggemma`",
                });
            }
        };
        let embedding_model = env
            .get("ESCUREL_EMBEDDING_MODEL")
            .or(toml_cfg.embedding.model);
        let embedding_device = pick("ESCUREL_EMBEDDING_DEVICE", toml_cfg.embedding.device, "cpu");
        let embedding_dim = match env.get("ESCUREL_EMBEDDING_DIM") {
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| ConfigError::InvalidValue {
                    var: "ESCUREL_EMBEDDING_DIM",
                    value: raw,
                    reason: "expected a positive integer",
                })?,
            None => toml_cfg.embedding.dim.unwrap_or(DEFAULT_DIM),
        };
        let gemini_api_key = env.get("ESCUREL_GEMINI_API_KEY");

        Ok(Self {
            version,
            env: server_env,
            data_dir,
            listen_http,
            tenant,
            storage_backend,
            s3,
            auth,
            embedding_provider,
            embedding_model,
            embedding_device,
            embedding_dim,
            gemini_api_key,
            demo_dir,
            seed_dir,
            webhook_url,
            webhook_secret,
            metrics_listen,
        })
    }
}

fn require_s3(
    env: &dyn EnvSource,
    var: &'static str,
    toml_val: Option<String>,
) -> Result<String, ConfigError> {
    env.get(var)
        .or(toml_val)
        .filter(|v| !v.is_empty())
        .ok_or(ConfigError::MissingS3Field { var })
}

/// A fully-wired, booted server plus the handles a long-running
/// process needs to keep alive (tempdirs, the reloadable embedder).
pub struct BootedServer {
    pub handle: crate::ServerHandle,
    /// The reloadable embedder seam — the `embedding_reload` admin
    /// RPC swaps a freshly-loaded model in here without restarting.
    pub embedder: Arc<ReloadableEmbedder>,
}

impl EscurelConfig {
    /// Build every backend the gateway needs and `serve()` it.
    ///
    /// Degraded start: if the configured embedder fails to load
    /// (missing model, no egress, …) this does **not** abort — it
    /// logs a warning, swaps in a [`ZeroEmbedder`] placeholder, and
    /// the returned server's `/readyz` reports `embedder: false`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for unrecoverable wiring failures (bad
    /// data dir, DuckDB open/migrate failure, S3 / Gemini config that
    /// the build's features can't satisfy). Embedder *load* failure is
    /// recoverable and does not error here.
    pub async fn build(&self) -> Result<BootedServer, ConfigError> {
        // 1. Data dir on the host volume.
        std::fs::create_dir_all(&self.data_dir).map_err(|source| ConfigError::DataDir {
            path: self.data_dir.display().to_string(),
            source,
        })?;

        // 2. LaneStore.
        let store = self.build_lane_store().await?;

        // 3. Embedder behind the reloadable seam. Load failure is
        //    degraded-start, not fatal.
        let embedder = Arc::new(self.build_embedder().await);

        // 4. Per-tenant DuckDB: open/create, migrate if fresh, build
        //    the indexer. A second connection on the same file backs
        //    the CRDT layer (the indexer owns its connection and does
        //    not expose it; the crdt_* tables it touches are disjoint
        //    from the indexer's pages/links/blocks, so the cross-table
        //    stale-read trap in the second-connection note does not
        //    apply here — see
        //    docs/notes/discovered/2026-05-26-server-binary-crdt-second-connection.md).
        // Validate the configured tenant id before it is joined into
        // a filesystem path. `ESCUREL_TENANT=../other` would
        // otherwise escape the tenant root and open a DuckDB file
        // outside it. The admin RPCs already gate tenant ids; the
        // binary's own configured tenant needs the same guard
        // (codex pre-v1 review).
        escurel_admin::validate_tenant_id(&self.tenant).map_err(|_| ConfigError::InvalidValue {
            var: "ESCUREL_TENANT",
            value: self.tenant.clone(),
            reason: "invalid tenant id (must be lowercase ascii / digit / '-' / '_', 1-64 chars)",
        })?;

        let tenant_dir = self.data_dir.join("tenants").join(&self.tenant);
        std::fs::create_dir_all(&tenant_dir).map_err(|source| ConfigError::DataDir {
            path: tenant_dir.display().to_string(),
            source,
        })?;
        let db_path = tenant_dir.join("escurel.duckdb");
        let fresh = !db_path.exists();
        let conn = Connection::open(&db_path).map_err(|source| ConfigError::DuckdbOpen {
            path: db_path.display().to_string(),
            source,
        })?;
        // `vss`/`fts` + the HNSW-persistence flag are per-connection session
        // state, so load them on EVERY boot — not only when the DB is fresh.
        // The schema DDL (`up`) is one-time, but a restart against an existing
        // DB still needs these extensions loaded on this write connection, or
        // modifying the HNSW-indexed `blocks` table fails ("unknown index type
        // 'HNSW'"). `INSTALL` is idempotent.
        Migrator::load_extensions(&conn)?;
        if fresh {
            Migrator::up(&conn)?;
        }

        let indexer = Arc::new(Indexer::new(
            Arc::clone(&store),
            Arc::clone(&embedder) as Arc<dyn Embedder>,
            conn,
            self.tenant.clone(),
        )?);

        // Cattle-node-loss recovery: when the DuckDB file was just
        // created but the LaneStore still holds canonical markdown
        // (fresh host / wiped local volume), rebuild the index from
        // that markdown so the server doesn't serve an empty corpus
        // until an operator runs the admin rebuild. On a genuine
        // first boot the store is empty and this is a fast no-op.
        // (codex pre-v1 review — the binary boot path must honour the
        // crash-recovery contract in docs/spec/storage.md, not just
        // the admin RPC.)
        if fresh {
            indexer.rebuild().await?;
        }

        // Optional seed: import a directory of markdown (e.g.
        // `examples/crm-demo`) into this tenant at boot. Idempotent
        // (upsert by body_hash), so it's safe to leave set across
        // restarts; powers the HTTP demo without manual fs placement.
        if let Some(dir) = self.seed_dir.as_ref() {
            indexer.seed_from_dir(dir).await?;
        }

        // Every served tenant ships the mandatory `escurel` meta-skill
        // — the agent's in-corpus navigation doc (locked decision 3,
        // docs/contract/agent-interface.md). Idempotent: a no-op when
        // the tenant already carries an `escurel` skill page.
        indexer.ensure_meta_skill().await?;

        // CRDT backend over a second connection to the same file.
        let crdt_conn = Connection::open(&db_path).map_err(|source| ConfigError::DuckdbOpen {
            path: db_path.display().to_string(),
            source,
        })?;
        // Same per-connection preamble for the CRDT backend's connection.
        Migrator::load_extensions(&crdt_conn)?;
        let crdt_backend: Arc<dyn CrdtBackend> =
            Arc::new(DuckdbCrdtBackend::new(Arc::new(Mutex::new(crdt_conn))));

        // 5. OIDC verifier (only when an issuer is configured).
        let verifier = self.build_verifier();

        // 6. Quota + tenant store.
        let quota = Some(Arc::new(QuotaManager::new(QuotaConfig::defaults())));
        let tenant_store = Arc::new(FsTenantStore::new(self.data_dir.join("tenants")));

        // 7. Readiness probe over the live dependencies.
        let readiness = Arc::new(DependencyProbe::new(
            Arc::clone(&store),
            Arc::clone(&embedder),
            self.tenant.clone(),
        ));

        let server_config = ServerConfig {
            listen: self.listen_http.clone(),
            version: self.version.clone(),
            readiness,
            indexer: Some(indexer),
            verifier,
            quota,
            tenant_store: Some(tenant_store),
            crdt_backend: Some(crdt_backend),
            // Hot-reload seam: the live embedder plus a factory that
            // rebuilds it from this config on demand. The
            // `embedding_reload` admin RPC retries a degraded-start
            // model load by calling the factory and swapping the
            // result into `embedder`.
            embedder_reload: Some(Arc::clone(&embedder)),
            embedder_factory: Some(self.embedder_factory()),
            demo_dir: self.demo_dir.clone(),
            webhook_url: self.webhook_url.clone(),
            webhook_secret: self.webhook_secret.clone(),
            metrics_listen: self.metrics_listen.clone(),
        };

        let handle = serve(server_config)
            .await
            .map_err(|e| ConfigError::InvalidValue {
                var: "ESCUREL_SERVER_LISTEN_HTTP",
                value: e.to_string(),
                reason: "failed to bind / serve",
            })?;

        Ok(BootedServer { handle, embedder })
    }

    async fn build_lane_store(&self) -> Result<Arc<dyn LaneStore>, ConfigError> {
        match self.storage_backend {
            StorageBackend::Fs => Ok(Arc::new(FsStore::new(self.data_dir.clone()))),
            StorageBackend::S3 => self.build_s3_store().await,
        }
    }

    #[cfg(feature = "s3")]
    async fn build_s3_store(&self) -> Result<Arc<dyn LaneStore>, ConfigError> {
        let s3 = self.s3.as_ref().ok_or(ConfigError::MissingS3Field {
            var: "ESCUREL_STORAGE_S3_BUCKET",
        })?;
        let store = escurel_storage::S3Store::new(escurel_storage::S3StoreConfig {
            bucket: s3.bucket.clone(),
            prefix: s3.prefix.clone(),
            endpoint_url: s3.endpoint.clone(),
            region: s3.region.clone(),
            access_key_id: s3.access_key_id.clone(),
            secret_access_key: s3.secret_access_key.clone(),
        })
        .await
        .map_err(|e| ConfigError::InvalidValue {
            var: "ESCUREL_STORAGE_S3_ENDPOINT",
            value: e.to_string(),
            reason: "failed to build S3 client",
        })?;
        Ok(Arc::new(store))
    }

    #[cfg(not(feature = "s3"))]
    async fn build_s3_store(&self) -> Result<Arc<dyn LaneStore>, ConfigError> {
        Err(ConfigError::EmbedderFeatureDisabled {
            provider: "s3-storage",
            feature: "s3",
        })
    }

    /// Build the embedder behind the reloadable seam. A load failure
    /// (real model missing / unreachable) is logged and degrades to a
    /// `ZeroEmbedder` placeholder rather than aborting the boot.
    async fn build_embedder(&self) -> ReloadableEmbedder {
        match self.load_real_embedder().await {
            Ok(inner) => ReloadableEmbedder::loaded(inner),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    provider = ?self.embedding_provider,
                    "embedder failed to load; booting degraded — /readyz embedder=false, \
                     retry via the embedding_reload admin RPC"
                );
                ReloadableEmbedder::degraded(self.embedding_dim)
            }
        }
    }

    /// Build the on-demand rebuild closure the `embedding_reload`
    /// admin tool invokes. It captures a clone of this config so the
    /// server layer never has to own the original `EscurelConfig`;
    /// each call re-attempts [`load_real_embedder`](Self::load_real_embedder)
    /// and, on success, returns the real embedder plus a revision
    /// label (the model id / path, falling back to the provider
    /// name) so the admin response can name which model is live.
    fn embedder_factory(&self) -> EmbedderFactory {
        let cfg = self.clone();
        Arc::new(move || {
            let cfg = cfg.clone();
            Box::pin(async move {
                let embedder = cfg.load_real_embedder().await.map_err(|e| e.to_string())?;
                Ok((embedder, cfg.embedder_revision()))
            })
        })
    }

    /// A short label naming the model that would load — the model
    /// id / path when set, otherwise the provider name. Surfaced as
    /// `EmbeddingReloadResponse.model_revision`.
    fn embedder_revision(&self) -> String {
        if let Some(model) = self.embedding_model.as_ref().filter(|m| !m.is_empty()) {
            return model.clone();
        }
        match self.embedding_provider {
            EmbeddingProvider::Zero => "zero".to_owned(),
            EmbeddingProvider::Gemini => "gemini".to_owned(),
            EmbeddingProvider::EmbeddingGemma => "embeddinggemma".to_owned(),
        }
    }

    /// Attempt to construct the configured real embedder. The
    /// candle / gemini paths are feature-gated; selecting a provider
    /// the build lacks is a recoverable (degraded-start) error.
    async fn load_real_embedder(&self) -> Result<Arc<dyn Embedder>, ConfigError> {
        match self.embedding_provider {
            EmbeddingProvider::Zero => Ok(Arc::new(ZeroEmbedder::new(self.embedding_dim))),
            EmbeddingProvider::Gemini => self.load_gemini(),
            EmbeddingProvider::EmbeddingGemma => self.load_embeddinggemma().await,
        }
    }

    #[cfg(feature = "gemini")]
    fn load_gemini(&self) -> Result<Arc<dyn Embedder>, ConfigError> {
        let key = self
            .gemini_api_key
            .clone()
            .filter(|k| !k.is_empty())
            .ok_or(ConfigError::MissingEmbedderField {
                provider: "gemini",
                var: "ESCUREL_GEMINI_API_KEY",
            })?;
        let mut e = escurel_embed::GeminiEmbedder::new(key).with_dim(self.embedding_dim);
        if let Some(model) = &self.embedding_model {
            e = e.with_model(model.clone());
        }
        Ok(Arc::new(e))
    }

    #[cfg(not(feature = "gemini"))]
    fn load_gemini(&self) -> Result<Arc<dyn Embedder>, ConfigError> {
        Err(ConfigError::EmbedderFeatureDisabled {
            provider: "gemini",
            feature: "gemini",
        })
    }

    #[cfg(feature = "embeddinggemma")]
    async fn load_embeddinggemma(&self) -> Result<Arc<dyn Embedder>, ConfigError> {
        let repo = self
            .embedding_model
            .clone()
            .unwrap_or_else(|| "google/embeddinggemma-300m".to_owned());
        // `from_hf_hub` is async (it fetches the weights into the HF
        // cache on a cold start); `build` is async so we await it
        // directly. Substrate production bakes the model into the
        // golden image, so the hub fetch is the dev / first-boot path.
        let loaded = escurel_embed::CandleEmbedder::from_hf_hub(&repo, self.embedding_dim)
            .await
            .map_err(|e| ConfigError::InvalidValue {
                var: "ESCUREL_EMBEDDING_MODEL",
                value: e.to_string(),
                reason: "failed to load EmbeddingGemma",
            })?;
        Ok(Arc::new(loaded))
    }

    #[cfg(not(feature = "embeddinggemma"))]
    async fn load_embeddinggemma(&self) -> Result<Arc<dyn Embedder>, ConfigError> {
        Err(ConfigError::EmbedderFeatureDisabled {
            provider: "embeddinggemma",
            feature: "embeddinggemma",
        })
    }

    fn build_verifier(&self) -> Option<Arc<OidcVerifier>> {
        let auth = self.auth.as_ref()?;
        let mut cfg = OidcConfig::new(auth.issuer.clone(), auth.audience.clone())
            .with_tenant_claim(auth.tenant_claim.clone())
            .with_admin_role(auth.admin_role_claim.clone(), auth.admin_role_value.clone());
        if let Some(uri) = auth.jwks_uri.clone() {
            cfg = cfg.with_jwks_uri(uri);
        }
        for (issuer, jwks_uri) in &auth.additional_issuers {
            cfg = cfg.with_additional_issuer(issuer.clone(), jwks_uri.clone());
        }
        cfg.jwks_refresh = auth.jwks_refresh;
        Some(Arc::new(OidcVerifier::new(cfg)))
    }
}
