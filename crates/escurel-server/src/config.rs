//! `ESCUREL_*` configuration surface + backend wiring for the
//! deployable `escurel-server` binary.
//!
//! [`EscurelConfig::from_env`] reads the 12-factor config surface
//! (principle 3): a TOML base file at `$ESCUREL_CONFIG` (optional)
//! overlaid by `ESCUREL_*` environment variables. The variable names
//! are the ones the substrate Kamal deploy contract pins
//! (`kamal/dz-escurel/deploy.yml` in the substrate repo, via Secret
//! Manager) and the spec config table
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
//! | `ESCUREL_REBUILD_INDEX_ON_BOOT` | `if-missing` | derived-index boot policy: `if-missing` (reuse an existing DuckDB; rebuild only when absent) or `always` (drop + rebuild from the markdown LaneStore each start; the container default — HNSW-persistence-reload workaround) |
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
//! | `ESCUREL_WRITE_ACL` | `off` | per-instance write ACL: `off` (no check) \| `log` (warn but allow) \| `enforce` (reject). Symmetric to the read ACL: owner-or-admin writes; public/no-owner instances are admin-write-only. |
//! | `ESCUREL_AUTH_ADMIN_ROLE_CLAIM` | `roles` | JWT claim listing roles |
//! | `ESCUREL_AUTH_ADMIN_ROLE_VALUE` | `escurel:admin` | role value granting admin |
//! | `ESCUREL_AUTH_JWKS_REFRESH_SECS` | `300` | JWKS cache TTL (seconds) |
//! | `ESCUREL_AUTH_JWKS_URI` | derived from issuer | explicit JWKS URL (e.g. Triton's `<issuer>/.well-known/jwks.json`) |
//! | `ESCUREL_AUTH_OIDC_ISSUER_2` | — | optional SECOND trusted issuer (e.g. Carl, for the dashboard's self-minted token); shares the audience + tenant claim |
//! | `ESCUREL_AUTH_JWKS_URI_2` | derived from issuer #2 | explicit JWKS URL for the second issuer (e.g. Carl's `<issuer>/jwks.json`) |
//! | `ESCUREL_AUTH_OIDC_ISSUER_3` (… `_N`) | — | further trusted issuers, read as a contiguous `_2.._N` sequence (e.g. `_3` = the escurel-explore BFF's browser auth bridge); a gap stops the scan |
//! | `ESCUREL_AUTH_JWKS_URI_3` (… `_N`) | derived from issuer #N | explicit JWKS URL for the Nth issuer |
//! | `ESCUREL_EMBEDDING_PROVIDER` | `gemini` | `zero`, `gemini`, or `embeddinggemma` (gemini with no key → zero fallback) |
//! | `ESCUREL_EMBEDDING_MODEL` | provider default | model id |
//! | `ESCUREL_EMBEDDING_DEVICE` | `cpu` | candle device (informational; CPU only today) |
//! | `ESCUREL_EMBEDDING_DIM` | `768` | vector dimension |
//! | `ESCUREL_GEMINI_API_KEY` | — | Gemini API key (provider=gemini; unset → zero fallback) |
//! | `ESCUREL_INDEX_BACKEND` | `single-file` | `single-file` or `ducklake` — selects the [`escurel_index::snapshot::IndexStore`] backend (DuckLake PR 6) |
//! | `ESCUREL_ROLE` | `writer` | `writer` or `reader` — `reader` requires `ESCUREL_INDEX_BACKEND=ducklake`; a reader boots with NO local single-file DuckDB, adopting the lake's newest published snapshot instead |
//! | `ESCUREL_DUCKLAKE_CATALOG_DSN` | — | DuckLake catalog DSN — a Postgres key/value DSN (contains `=`) or a DuckDB-file catalog path; required when `ESCUREL_INDEX_BACKEND=ducklake` |
//! | `ESCUREL_DUCKLAKE_DATA_PATH` | — | DuckLake `DATA_PATH` — `gs://…`, `s3://…`, or a local directory; required when `ESCUREL_INDEX_BACKEND=ducklake` |
//! | `ESCUREL_DUCKLAKE_GCS_KEY_ID` / `ESCUREL_DUCKLAKE_GCS_SECRET` | — | GCS HMAC key pair; required when `ESCUREL_DUCKLAKE_DATA_PATH` starts with `gs://` |
//! | `ESCUREL_DUCKLAKE_S3_ENDPOINT` / `_S3_ACCESS_KEY_ID` / `_S3_SECRET_ACCESS_KEY` / `_S3_REGION` | — / — / — / `us-east-1` | S3 (or MinIO) credentials; required when `ESCUREL_DUCKLAKE_DATA_PATH` starts with `s3://` |
//! | `ESCUREL_DUCKLAKE_S3_USE_SSL` | `true` | whether the S3/MinIO endpoint above is TLS |
//! | `ESCUREL_SNAPSHOT_REFRESH_SECS` | `30` | a reader's background lake-poll interval (seconds); see `escurel_server::snapshot_refresh::RefreshTask` |
//! | `ESCUREL_SNAPSHOT_PUBLISH_SECS` | `0` | a writer's optional periodic publish interval (seconds); `0` disables it (manual-only, via the `publish_snapshot` admin tool) — see `escurel_server::snapshot_publish::PublishTask` |
//! | `ESCUREL_SNAPSHOT_KEEP` | `5` | how many DuckLake snapshots to retain after a successful publish; the GC pass never touches the current snapshot |

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use duckdb::Connection;
use escurel_admin::FsTenantStore;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_embed::{Embedder, ReloadableEmbedder, ZeroEmbedder};
use escurel_index::backend::ContextualizeMode;
use escurel_index::snapshot::{
    AttachRetrievalFn, IndexStore, LakeConfig, ObjectStoreSecret, SingleFileStore, SnapshotError,
    adopt_lake,
};
use escurel_index::{Indexer, IndexerHandle};
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

/// When to drop + rebuild the derived DuckDB index at boot
/// (`ESCUREL_REBUILD_INDEX_ON_BOOT`).
///
/// The DuckDB file is a *derived* cache reconstructable from the canonical
/// markdown LaneStore. `Always` drops it on every start so the process
/// rebuilds a fresh index — the container default, because `vss`'s
/// experimental HNSW persistence segfaults when a restarted process reloads
/// the on-disk index (see the Dockerfile note). `IfMissing` (the binary
/// default) keeps an existing index and only rebuilds when the file is
/// absent (a fresh host / wiped volume) — fast restarts, no re-embed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildIndexOnBoot {
    /// Drop the derived DuckDB at boot and rebuild from the LaneStore.
    Always,
    /// Keep an existing derived DuckDB; rebuild only when it is missing.
    IfMissing,
}

/// `IndexStore` backend selector (`ESCUREL_INDEX_BACKEND`, DuckLake PR 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexBackend {
    /// The classic one-DuckDB-file-per-tenant backend
    /// ([`SingleFileStore`]). The only backend before this PR.
    SingleFile,
    /// A Postgres-catalog DuckLake, published by a writer and adopted
    /// by readers (`escurel_index::snapshot::{attach_lake, adopt_lake}`).
    DuckLake,
}

/// This instance's role (`ESCUREL_ROLE`, DuckLake PR 6). A `Reader`
/// requires [`IndexBackend::DuckLake`] — there is no such thing as a
/// single-file reader, because a single-file DuckDB has exactly one
/// writer-serving instance by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRole {
    /// Owns the canonical index; may attach + publish a DuckLake.
    Writer,
    /// Serves a read-only copy adopted from a published DuckLake
    /// snapshot; no local single-file DuckDB, no CRDT/chat surface.
    Reader,
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

/// Parse an `embedding_provider` string (#247 tenant spec / env) into the
/// selector. `None` for an unknown value (caller keeps the current provider).
pub(crate) fn parse_embedding_provider(s: &str) -> Option<EmbeddingProvider> {
    match s {
        "zero" => Some(EmbeddingProvider::Zero),
        "gemini" => Some(EmbeddingProvider::Gemini),
        "embeddinggemma" => Some(EmbeddingProvider::EmbeddingGemma),
        _ => None,
    }
}

/// Fold a per-tenant [`QuotaOverride`](escurel_types::QuotaOverride) onto a
/// base [`QuotaConfig`] — each `Some` field wins, `None` inherits (#247).
pub(crate) fn apply_quota_override(
    mut base: QuotaConfig,
    over: escurel_types::QuotaOverride,
) -> QuotaConfig {
    if let Some(v) = over.queries_per_minute {
        base.queries_per_minute = v;
    }
    if let Some(v) = over.writes_per_minute {
        base.writes_per_minute = v;
    }
    if let Some(v) = over.embeds_per_minute {
        base.embeds_per_minute = v;
    }
    if let Some(v) = over.concurrent_sessions {
        base.concurrent_sessions = v;
    }
    if let Some(v) = over.max_blob_bytes {
        base.max_blob_bytes = v;
    }
    base
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
    #[error("{var} is required when ESCUREL_INDEX_BACKEND=ducklake")]
    MissingLakeField { var: &'static str },
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
    /// The CRDT op-log re-homing attach (`DuckdbCrdtBackend::
    /// attach_shared_pg`, DuckLake PR 10) failed on the session-actor
    /// connection.
    #[error("crdt pg attach failed: {0}")]
    CrdtPg(#[from] escurel_crdt::Error),
}

/// Map an [`IndexStore`] open failure back onto the exact
/// [`ConfigError`] variants the inline boot used to produce, so the
/// fatal-boot error surface is unchanged by the seam extraction.
impl From<SnapshotError> for ConfigError {
    fn from(e: SnapshotError) -> Self {
        match e {
            SnapshotError::DataDir { path, source } => ConfigError::DataDir { path, source },
            SnapshotError::DuckdbOpen { path, source } => ConfigError::DuckdbOpen { path, source },
            SnapshotError::Migrate(m) => ConfigError::Migrate(m),
            SnapshotError::Indexer(i) => ConfigError::Indexer(i),
            // Unreachable from `SingleFileStore::open`; kept total so the
            // `?` conversion compiles for any IndexStore backend.
            SnapshotError::Unsupported(reason) => ConfigError::InvalidValue {
                var: "ESCUREL_SERVER_DATA_DIR",
                value: String::new(),
                reason,
            },
            // DuckLake variants (PR 3) — also unreachable from
            // `SingleFileStore::open`; the server grows a lake config
            // surface in a later PR (5-6). Mapped mechanically so the
            // conversion stays total.
            SnapshotError::InvalidLakeConfig(value) => ConfigError::InvalidValue {
                var: "ESCUREL_LAKE",
                value,
                reason: "invalid lake config",
            },
            SnapshotError::LakeSql(source) => ConfigError::DuckdbOpen {
                path: "ducklake".to_owned(),
                source,
            },
            SnapshotError::LakeIncompatible(value) => ConfigError::InvalidValue {
                var: "ESCUREL_LAKE",
                value,
                reason: "lake incompatible with this reader",
            },
            // DuckLake PR 10 — the indexer's OWN crdt-pg attach
            // (`Indexer::attach_crdt_pg`) failing on the indexer's own
            // connection maps onto the same `ConfigError::CrdtPg` variant
            // as `DuckdbCrdtBackend::attach_shared_pg`'s failure below.
            SnapshotError::Crdt(e) => ConfigError::CrdtPg(e),
        }
    }
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
    retrieval: TomlRetrieval,
    #[serde(default)]
    observability: TomlObservability,
    #[serde(default)]
    ingest: TomlIngest,
}

#[derive(Debug, Default, Deserialize)]
struct TomlServer {
    data_dir: Option<String>,
    listen_http: Option<String>,
    tenant: Option<String>,
    version: Option<String>,
    env: Option<String>,
    rebuild_index_on_boot: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TomlAuth {
    oidc_issuer: Option<String>,
    oidc_audience: Option<String>,
    tenant_claim: Option<String>,
    admin_role_claim: Option<String>,
    admin_role_value: Option<String>,
    groups_claim: Option<String>,
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
struct TomlRetrieval {
    rerank: Option<String>,
    rerank_candidates: Option<usize>,
    rerank_model: Option<String>,
    rerank_device: Option<String>,
    two_pass: Option<bool>,
    coarse_dim: Option<usize>,
    coarse_candidates: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct TomlObservability {
    metrics_listen: Option<String>,
}

/// `[ingest]` TOML section. Document-ingest knobs. Only the contextual
/// retrieval mode (GH #216, Variant A) is modelled today.
#[derive(Debug, Default, Deserialize)]
struct TomlIngest {
    /// `"off"` | `"structural"`. Overlaid by `ESCUREL_INGEST_CONTEXTUALIZE`.
    contextualize: Option<String>,
}

/// Second-stage rerank selector. `Off` returns the first-stage fused
/// order; `Bge` loads the candle cross-encoder ([`escurel_embed::CrossEncoderReranker`],
/// behind the `rerank` build feature). The runtime default is `Bge` when the
/// binary is built `--features rerank`, else `Off` — "default-on where built".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankMode {
    Off,
    Bge,
}

/// Auth config (present only when an OIDC issuer is set).
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub issuer: String,
    pub audience: String,
    pub tenant_claim: String,
    pub admin_role_claim: String,
    pub admin_role_value: String,
    /// JWT claim listing the subject's groups for the data-level ACL.
    /// Defaults to the same `roles` claim admin derives from.
    pub groups_claim: String,
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
    /// Second-stage rerank mode (`[retrieval].rerank`, default-on where the
    /// `rerank` feature is built). `Off` reproduces first-stage-only ranking.
    pub rerank_mode: RerankMode,
    /// How many fused candidates feed the reranker (`rerank_candidates`).
    pub rerank_candidates: usize,
    /// Reranker model — an HF repo id (e.g. `BAAI/bge-reranker-v2-m3`) or a
    /// local directory holding `config.json` + `tokenizer.json` +
    /// `model.safetensors` (the air-gapped substrate bake).
    pub rerank_model: String,
    /// Inference device for the reranker (informational; candle is CPU today).
    pub rerank_device: String,
    /// Matryoshka two-pass vector search (`[retrieval].two_pass`, issue #218).
    /// `false` (default) keeps single-pass full-dimension search.
    pub two_pass: bool,
    /// Truncated dimension for the two-pass coarse shortlist (`coarse_dim`).
    pub coarse_dim: usize,
    /// Coarse-pass shortlist size handed to the full-dim rescore
    /// (`coarse_candidates`).
    pub coarse_candidates: usize,
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
    /// Optional shared secret signing/verifying skill packs
    /// (`ESCUREL_PACK_SECRET`, REQ-PACK-02). `None` (default) disables
    /// the pack surface fail-closed: `export_pack` refuses rather than
    /// emit an unverifiable bundle.
    pub pack_secret: Option<String>,
    /// Dedicated Prometheus `/metrics` listener
    /// (`ESCUREL_OBSERVABILITY_METRICS_LISTEN`, default
    /// `0.0.0.0:9090`). `None` when explicitly emptied — disables
    /// scraping.
    pub metrics_listen: Option<String>,
    /// Document contextual-retrieval mode (GH #216, Variant A). Set from
    /// `ESCUREL_INGEST_CONTEXTUALIZE` (`off` | `structural`); default
    /// `structural`. Threaded into the per-tenant `Indexer` so both the live
    /// ingest worker and the rebuild path situate chunks with a
    /// `[<title> › <heading path> › p.<page>]` context (stored beside the
    /// verbatim body; feeds the dense/FTS/rerank representations only).
    pub ingest_contextualize: ContextualizeMode,
    /// Whether to drop + rebuild the derived DuckDB index at boot
    /// (`ESCUREL_REBUILD_INDEX_ON_BOOT`; default `if-missing`). The container
    /// image sets `always` to sidestep the HNSW-persistence-reload segfault.
    pub rebuild_index_on_boot: RebuildIndexOnBoot,
    /// `IndexStore` backend selector (`ESCUREL_INDEX_BACKEND`, default
    /// `single-file`, DuckLake PR 6).
    pub index_backend: IndexBackend,
    /// This instance's role (`ESCUREL_ROLE`, default `writer`).
    pub role: ServerRole,
    /// The DuckLake this instance attaches to/adopts from. `Some` iff
    /// `index_backend == DuckLake`; validated + built from the
    /// `ESCUREL_DUCKLAKE_*` vars at [`EscurelConfig::from_env`] time.
    pub lake: Option<LakeConfig>,
    /// A reader's background lake-poll interval, seconds
    /// (`ESCUREL_SNAPSHOT_REFRESH_SECS`, default `30`). Unused by a
    /// writer or the single-file backend.
    pub snapshot_refresh_secs: u64,
    /// A writer's optional periodic publish interval, seconds
    /// (`ESCUREL_SNAPSHOT_PUBLISH_SECS`, default `0` = disabled,
    /// manual-only via the `publish_snapshot` admin tool). Unused by a
    /// reader or the single-file backend.
    pub snapshot_publish_secs: u64,
    /// DuckLake snapshot retention count (`ESCUREL_SNAPSHOT_KEEP`,
    /// default `5`). `0` disables the GC pass a successful publish runs
    /// afterwards.
    pub snapshot_keep: u32,
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
        // Optional shared secret signing/verifying skill packs. An
        // empty value is treated as unset — the pack surface stays off.
        let pack_secret = env
            .get("ESCUREL_PACK_SECRET")
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
        // Derived-index boot policy. `if-missing` (default) reuses an existing
        // DuckDB; `always` drops it and rebuilds from the LaneStore (the
        // container default — HNSW-persistence-reload segfault workaround).
        let rebuild_raw = pick(
            "ESCUREL_REBUILD_INDEX_ON_BOOT",
            toml_cfg.server.rebuild_index_on_boot,
            "if-missing",
        );
        let rebuild_index_on_boot = match rebuild_raw.trim().to_ascii_lowercase().as_str() {
            "always" => RebuildIndexOnBoot::Always,
            "if-missing" | "if_missing" | "missing" => RebuildIndexOnBoot::IfMissing,
            other => {
                return Err(ConfigError::InvalidValue {
                    var: "ESCUREL_REBUILD_INDEX_ON_BOOT",
                    value: other.to_owned(),
                    reason: "must be `always` or `if-missing`",
                });
            }
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
                    groups_claim: pick(
                        "ESCUREL_AUTH_GROUPS_CLAIM",
                        toml_cfg.auth.groups_claim,
                        "roles",
                    ),
                    jwks_refresh: Duration::from_secs(jwks_refresh_secs),
                    jwks_uri: env
                        .get("ESCUREL_AUTH_JWKS_URI")
                        .filter(|s| !s.is_empty())
                        .or(toml_cfg.auth.jwks_uri),
                    // Optional additional trusted issuers (additive; absent →
                    // single-issuer). Read as a contiguous `_2.._N` sequence —
                    // ISSUER_2 (Carl, dashboard self-mint), ISSUER_3 (the
                    // escurel-explore BFF, browser auth bridge), and so on.
                    // The first gap stops the scan, so a stray ISSUER_3 with no
                    // ISSUER_2 is a misconfiguration and is not silently
                    // promoted. Each entry's JWKS URI is explicit when set,
                    // else derived from the issuer by the verifier.
                    additional_issuers: {
                        let mut extra = Vec::new();
                        let mut n = 2;
                        while let Some(issuer_n) = env
                            .get(&format!("ESCUREL_AUTH_OIDC_ISSUER_{n}"))
                            .filter(|s| !s.is_empty())
                        {
                            let jwks_n = env
                                .get(&format!("ESCUREL_AUTH_JWKS_URI_{n}"))
                                .filter(|s| !s.is_empty());
                            extra.push((issuer_n, jwks_n));
                            n += 1;
                        }
                        extra
                    },
                })
            }
            _ => None,
        };

        // --- embedding ---
        // Default: hosted Gemini (the binary ships the `gemini` feature). With
        // no API key it falls back to ZeroEmbedder (see `load_gemini`), so
        // keyless dev/CI/air-gapped boots stay clean; air-gapped deployments
        // set `embeddinggemma` (local) or `zero` explicitly.
        let provider_str = pick(
            "ESCUREL_EMBEDDING_PROVIDER",
            toml_cfg.embedding.provider,
            "gemini",
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

        // --- ingest (GH #216, Variant A) ---
        // Default `structural`; `off` restores verbatim chunk text. Unknown
        // values fall back to the default (parse is lenient by design).
        let ingest_contextualize = ContextualizeMode::parse(&pick(
            "ESCUREL_INGEST_CONTEXTUALIZE",
            toml_cfg.ingest.contextualize,
            "structural",
        ));

        // --- retrieval (rerank) ---
        // Default-on where built: a `--features rerank` binary defaults to the
        // bge cross-encoder; a default (rerank-less) binary defaults to `off`.
        let default_rerank = if cfg!(feature = "rerank") {
            "bge"
        } else {
            "off"
        };
        let rerank_str = pick(
            "ESCUREL_RETRIEVAL_RERANK",
            toml_cfg.retrieval.rerank,
            default_rerank,
        );
        let rerank_mode = match rerank_str.as_str() {
            "off" => RerankMode::Off,
            "bge" => RerankMode::Bge,
            other => {
                return Err(ConfigError::InvalidValue {
                    var: "ESCUREL_RETRIEVAL_RERANK",
                    value: other.to_owned(),
                    reason: "expected `off` or `bge`",
                });
            }
        };
        let rerank_candidates = match env.get("ESCUREL_RETRIEVAL_RERANK_CANDIDATES") {
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| ConfigError::InvalidValue {
                    var: "ESCUREL_RETRIEVAL_RERANK_CANDIDATES",
                    value: raw,
                    reason: "expected a positive integer",
                })?,
            None => toml_cfg.retrieval.rerank_candidates.unwrap_or(100),
        };
        let rerank_model = pick(
            "ESCUREL_RETRIEVAL_RERANK_MODEL",
            toml_cfg.retrieval.rerank_model,
            "BAAI/bge-reranker-v2-m3",
        );
        let rerank_device = pick(
            "ESCUREL_RETRIEVAL_RERANK_DEVICE",
            toml_cfg.retrieval.rerank_device,
            "cpu",
        );

        // Matryoshka two-pass (issue #218): off by default. `ESCUREL_RETRIEVAL_TWO_PASS`
        // accepts `true`/`1`/`yes`/`on` (case-insensitive); anything else is false.
        let two_pass = match env.get("ESCUREL_RETRIEVAL_TWO_PASS") {
            Some(raw) => matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            ),
            None => toml_cfg.retrieval.two_pass.unwrap_or(false),
        };
        let coarse_dim = match env.get("ESCUREL_RETRIEVAL_COARSE_DIM") {
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| ConfigError::InvalidValue {
                    var: "ESCUREL_RETRIEVAL_COARSE_DIM",
                    value: raw,
                    reason: "expected a positive integer (a Matryoshka prefix, e.g. 128|256|512)",
                })?,
            None => toml_cfg.retrieval.coarse_dim.unwrap_or(128),
        };
        let coarse_candidates = match env.get("ESCUREL_RETRIEVAL_COARSE_CANDIDATES") {
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| ConfigError::InvalidValue {
                    var: "ESCUREL_RETRIEVAL_COARSE_CANDIDATES",
                    value: raw,
                    reason: "expected a positive integer",
                })?,
            None => toml_cfg.retrieval.coarse_candidates.unwrap_or(500),
        };

        // --- DuckLake backend / role (PR 6) ---
        let role = match env
            .get("ESCUREL_ROLE")
            .unwrap_or_else(|| "writer".to_owned())
            .as_str()
        {
            "writer" => ServerRole::Writer,
            "reader" => ServerRole::Reader,
            other => {
                return Err(ConfigError::InvalidValue {
                    var: "ESCUREL_ROLE",
                    value: other.to_owned(),
                    reason: "expected `writer` or `reader`",
                });
            }
        };
        let index_backend = match env
            .get("ESCUREL_INDEX_BACKEND")
            .unwrap_or_else(|| "single-file".to_owned())
            .as_str()
        {
            "single-file" => IndexBackend::SingleFile,
            "ducklake" => IndexBackend::DuckLake,
            other => {
                return Err(ConfigError::InvalidValue {
                    var: "ESCUREL_INDEX_BACKEND",
                    value: other.to_owned(),
                    reason: "expected `single-file` or `ducklake`",
                });
            }
        };
        // Fail closed: a single-file DuckDB has exactly one
        // writer-serving instance by construction — "reader" only makes
        // sense against a DuckLake.
        if role == ServerRole::Reader && index_backend != IndexBackend::DuckLake {
            return Err(ConfigError::InvalidValue {
                var: "ESCUREL_ROLE",
                value: "reader".to_owned(),
                reason: "reader role requires ESCUREL_INDEX_BACKEND=ducklake",
            });
        }
        let lake = match index_backend {
            IndexBackend::DuckLake => Some(build_lake_config(env)?),
            IndexBackend::SingleFile => None,
        };
        let snapshot_refresh_secs = match env.get("ESCUREL_SNAPSHOT_REFRESH_SECS") {
            Some(raw) => raw.parse::<u64>().map_err(|_| ConfigError::InvalidValue {
                var: "ESCUREL_SNAPSHOT_REFRESH_SECS",
                value: raw,
                reason: "expected a non-negative integer (seconds)",
            })?,
            None => 30,
        };
        let snapshot_publish_secs = match env.get("ESCUREL_SNAPSHOT_PUBLISH_SECS") {
            Some(raw) => raw.parse::<u64>().map_err(|_| ConfigError::InvalidValue {
                var: "ESCUREL_SNAPSHOT_PUBLISH_SECS",
                value: raw,
                reason: "expected a non-negative integer (seconds); 0 disables the periodic publish task",
            })?,
            None => 0,
        };
        let snapshot_keep = match env.get("ESCUREL_SNAPSHOT_KEEP") {
            Some(raw) => raw.parse::<u32>().map_err(|_| ConfigError::InvalidValue {
                var: "ESCUREL_SNAPSHOT_KEEP",
                value: raw,
                reason: "expected a non-negative integer; 0 disables snapshot GC",
            })?,
            None => 5,
        };

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
            rerank_mode,
            rerank_candidates,
            rerank_model,
            rerank_device,
            two_pass,
            coarse_dim,
            coarse_candidates,
            demo_dir,
            seed_dir,
            webhook_url,
            webhook_secret,
            pack_secret,
            metrics_listen,
            ingest_contextualize,
            rebuild_index_on_boot,
            index_backend,
            role,
            lake,
            snapshot_refresh_secs,
            snapshot_publish_secs,
            snapshot_keep,
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

/// Required-when-ducklake env lookup, mirroring [`require_s3`].
fn require_lake(env: &dyn EnvSource, var: &'static str) -> Result<String, ConfigError> {
    env.get(var)
        .filter(|v| !v.is_empty())
        .ok_or(ConfigError::MissingLakeField { var })
}

/// Build a [`LakeConfig`] from the `ESCUREL_DUCKLAKE_*` vars. The catalog
/// DSN and DATA_PATH are always required; the object-store credential set
/// is picked from the `DATA_PATH` scheme — `gs://` needs the GCS pair,
/// `s3://` needs the S3 quadruple, anything else (a local directory) needs
/// neither. Splice/shape validation (safe characters, local-dir
/// existence, secret/scheme agreement) happens later, in
/// `escurel_index::snapshot::lake::validate` — this function only decides
/// WHICH fields are required, not whether their values are well-formed.
fn build_lake_config(env: &dyn EnvSource) -> Result<LakeConfig, ConfigError> {
    let catalog_dsn = require_lake(env, "ESCUREL_DUCKLAKE_CATALOG_DSN")?;
    let data_path = require_lake(env, "ESCUREL_DUCKLAKE_DATA_PATH")?;
    let object_store = if data_path.starts_with("gs://") {
        ObjectStoreSecret::Gcs {
            key_id: require_lake(env, "ESCUREL_DUCKLAKE_GCS_KEY_ID")?,
            secret: require_lake(env, "ESCUREL_DUCKLAKE_GCS_SECRET")?,
        }
    } else if data_path.starts_with("s3://") {
        let use_ssl = match env.get("ESCUREL_DUCKLAKE_S3_USE_SSL") {
            Some(raw) => matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            ),
            None => true,
        };
        ObjectStoreSecret::S3 {
            endpoint: require_lake(env, "ESCUREL_DUCKLAKE_S3_ENDPOINT")?,
            access_key_id: require_lake(env, "ESCUREL_DUCKLAKE_S3_ACCESS_KEY_ID")?,
            secret_access_key: require_lake(env, "ESCUREL_DUCKLAKE_S3_SECRET_ACCESS_KEY")?,
            region: env
                .get("ESCUREL_DUCKLAKE_S3_REGION")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "us-east-1".to_owned()),
            use_ssl,
        }
    } else {
        ObjectStoreSecret::None
    };
    Ok(LakeConfig {
        catalog_dsn,
        data_path,
        object_store,
    })
}

/// A fully-wired, booted server plus the handles a long-running
/// process needs to keep alive (tempdirs, the reloadable embedder).
pub struct BootedServer {
    pub handle: crate::ServerHandle,
    /// The reloadable embedder seam — the `embedding_reload` admin
    /// RPC swaps a freshly-loaded model in here without restarting.
    pub embedder: Arc<ReloadableEmbedder>,
    /// A ducklake reader's background poll/adopt/hot-swap loop
    /// (DuckLake PR 6). `Some` only for `ESCUREL_INDEX_BACKEND=ducklake`
    /// together with `ESCUREL_ROLE=reader`; the caller (`main.rs`) must
    /// shut it down alongside `handle` on SIGTERM so the task doesn't
    /// outlive the process's other background work.
    pub refresh_handle: Option<crate::snapshot_refresh::RefreshHandle>,
    /// A ducklake writer's optional periodic publish loop (DuckLake PR
    /// 7). `Some` only for `ESCUREL_INDEX_BACKEND=ducklake` +
    /// `ESCUREL_ROLE=writer` + `ESCUREL_SNAPSHOT_PUBLISH_SECS > 0`; the
    /// caller must shut it down alongside `handle` on SIGTERM, same as
    /// `refresh_handle`.
    pub publish_handle: Option<crate::snapshot_publish::PublishHandle>,
}

/// The five backend handles `EscurelConfig::build`'s (index_backend,
/// role) match produces: the hot-swap seam, the optional CRDT backend
/// (writer-only), whether this boot is a ducklake reader, and the two
/// optional background task handles (a reader's poll loop / a writer's
/// optional periodic publish loop).
type BootIndex = (
    IndexerHandle,
    Option<Arc<dyn CrdtBackend>>,
    bool,
    Option<crate::snapshot_refresh::RefreshHandle>,
    Option<crate::snapshot_publish::PublishHandle>,
);

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

        // #247: the served tenant's spec is the source of truth for its
        // embedding provider, quota override, and lifecycle status. Load it
        // (if any) so the embedder honours the tenant's declared provider —
        // not just the gateway env default — and quotas/status take effect.
        let tenant_spec = self.load_served_tenant_spec().await;

        // 3. Embedder behind the reloadable seam. Load failure is
        //    degraded-start, not fatal. The effective config folds in the
        //    tenant's `embedding_provider` when it declares one.
        let embed_cfg = self.with_tenant_embedding(tenant_spec.as_ref());
        let embedder = Arc::new(embed_cfg.build_embedder().await);

        // 4. Per-tenant DuckDB via the `IndexStore` seam (DuckLake PR 2).
        //    `SingleFileStore::open()` reproduces the classic boot sequence
        //    verbatim: open/create the file, migrate if fresh, `try_clone`
        //    the CRDT connection off the same instance, build the indexer,
        //    fresh-only rebuild, optional seed, meta-skill. See
        //    `escurel_index::snapshot::SingleFileStore` (and
        //    docs/notes/discovered/2026-05-26-server-binary-crdt-second-connection.md
        //    for why the CRDT connection must be a clone, not a re-open).
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

        // Retrieval-attach hook: runs inside `open()` at exactly the point
        // the inline boot used to call `attach_retrieval` (after
        // `Indexer::new`, before the fresh-boot rebuild). It attaches a
        // second-stage cross-encoder when `[retrieval].rerank` is on (issue
        // #215) and/or Matryoshka two-pass vector search when
        // `[retrieval].two_pass` is on (issue #218). A reranker load failure
        // is degraded-start — log + run without rerank, never fatal —
        // mirroring the embedder.
        let attach_cfg = self.clone();
        let attach: AttachRetrievalFn = Arc::new(move |base: Indexer| {
            let cfg = attach_cfg.clone();
            Box::pin(async move { cfg.attach_retrieval(base).await })
        });

        let single_file = SingleFileStore {
            tenant_dir,
            // `Always` drops the DuckDB (a rebuildable cache) + its WAL — the
            // container default, replacing the old shell `rm *.duckdb`
            // ENTRYPOINT hack. `IfMissing` (default) keeps an existing index
            // for a fast, re-embed-free restart.
            rebuild_on_boot: self.rebuild_index_on_boot == RebuildIndexOnBoot::Always,
            store: Arc::clone(&store),
            embedder: Arc::clone(&embedder) as Arc<dyn Embedder>,
            tenant: self.tenant.clone(),
            contextualize: self.ingest_contextualize,
            attach_retrieval: Some(attach),
            seed_dir: self.seed_dir.clone(),
        };
        // Branch on (index_backend, role) — the DuckLake PR 6 decision
        // table:
        //   (SingleFile, *)     → today's boot, unchanged (PR 2's
        //                         zero-behaviour-change guarantee).
        //   (DuckLake, Writer)  → today's boot PLUS an idempotent `ATTACH`
        //                         on the indexer's own connection, so a
        //                         later publish (an admin tool — NOT built
        //                         in this PR) never pays a fresh attach.
        //                         The writer still owns bootstrap
        //                         (CRDT/chat, seed, meta-skill); re-homing
        //                         those off the writer is Phase B, out of
        //                         scope here.
        //   (DuckLake, Reader)  → NO local single-file DuckDB. A
        //                         synchronous `adopt_lake` runs BEFORE the
        //                         HTTP listener binds — boot fails if the
        //                         lake has never been published or is
        //                         incompatible, the same "not available
        //                         until built" semantics the single-file
        //                         path already has — then a `RefreshTask`
        //                         keeps the served snapshot current
        //                         without a restart. No CRDT/chat/seed/
        //                         meta-skill (Phase B, again out of scope).
        // Shared between the `publish_snapshot` admin tool and an
        // optional periodic `PublishTask` (DuckLake PR 7) so neither
        // re-publishes an epoch the other just published. Built
        // unconditionally — inert (never read) on every non-ducklake-
        // writer boot shape.
        let last_published_epoch = Arc::new(std::sync::Mutex::new(None));
        let (indexer_handle, crdt_backend, reader_mode, refresh_handle, publish_handle): BootIndex =
            match (self.index_backend, self.role) {
                (IndexBackend::DuckLake, ServerRole::Reader) => {
                    let lake_cfg = self.lake.as_ref().expect(
                        "ESCUREL_INDEX_BACKEND=ducklake always carries a LakeConfig \
                     (EscurelConfig::from_env validated this)",
                    );
                    let adopted = adopt_lake(
                        lake_cfg,
                        Arc::clone(&store),
                        Arc::clone(&embedder) as Arc<dyn Embedder>,
                        &self.tenant,
                        None,
                    )
                    .await?
                    .ok_or_else(|| ConfigError::InvalidValue {
                        var: "ESCUREL_DUCKLAKE_CATALOG_DSN",
                        value: lake_cfg.catalog_dsn.clone(),
                        reason: "lake has never been published; a reader cannot boot from an \
                             empty lake — publish from a writer first",
                    })?;
                    // Phase B (DuckLake PR 8): a reader gets read-write
                    // access to the shared chat Postgres table, reusing
                    // `catalog_dsn` — no separate config. Only when the
                    // catalog is genuinely Postgres (`is_pg_catalog`); a
                    // DuckDB-file catalog (dev/test-only shape; production
                    // per ADR-0009 is always Postgres) has no Postgres
                    // database to attach into, so chat falls back to the
                    // adopted-but-ephemeral local `chat_messages` table in
                    // that shape — acceptable, it never has durable rows
                    // to begin with on a reader.
                    // Phase B (DuckLake PR 9): the reader ALSO gets
                    // read-write access to the shared events Postgres
                    // table — same rationale, same `is_pg_catalog` gate,
                    // same `catalog_dsn` reuse as chat above.
                    // Phase B (DuckLake PR 10): the reader ALSO gets a
                    // live CRDT backend over the shared op-log/snapshot
                    // Postgres tables — same `is_pg_catalog` gate, same
                    // `catalog_dsn` reuse. TWO independent attaches, per
                    // `Indexer::attach_crdt_pg`'s doc: one on the
                    // indexer's own connection (so `list_snapshots` can
                    // read it), one on a fresh connection dedicated to the
                    // `DuckdbCrdtBackend` session-actor path — mirrors the
                    // writer's "own connection for chat/events, separate
                    // `crdt_conn` for the session actor" split, just
                    // without a local single-file connection to clone
                    // from (a reader has none — CRDT here never touches
                    // local tables, only the attached Postgres ones, so a
                    // bare in-memory connection is sufficient).
                    let mut reader_crdt_backend: Option<Arc<dyn CrdtBackend>> = None;
                    if lake_cfg.is_pg_catalog() {
                        adopted
                            .indexer
                            .attach_chat_pg(&lake_cfg.catalog_dsn)
                            .await?;
                        adopted
                            .indexer
                            .attach_events_pg(&lake_cfg.catalog_dsn)
                            .await?;
                        adopted
                            .indexer
                            .attach_crdt_pg(&lake_cfg.catalog_dsn)
                            .await?;

                        let crdt_conn = Connection::open_in_memory().map_err(|source| {
                            ConfigError::DuckdbOpen {
                                path: "<in-memory ducklake-reader crdt connection>".to_owned(),
                                source,
                            }
                        })?;
                        let backend = DuckdbCrdtBackend::new(Arc::new(Mutex::new(crdt_conn)));
                        backend
                            .attach_shared_pg(&lake_cfg.catalog_dsn, &self.tenant)
                            .await
                            .map_err(ConfigError::CrdtPg)?;
                        reader_crdt_backend = Some(Arc::new(backend));
                    }
                    let handle = IndexerHandle::fixed(adopted.indexer);
                    let refresh = crate::snapshot_refresh::RefreshTask::new(
                        handle.clone(),
                        lake_cfg.clone(),
                        Arc::clone(&store),
                        Arc::clone(&embedder) as Arc<dyn Embedder>,
                        self.tenant.clone(),
                        Duration::from_secs(self.snapshot_refresh_secs),
                        Some(adopted.snapshot_id),
                    )
                    .spawn();
                    (handle, reader_crdt_backend, true, Some(refresh), None)
                }
                (backend, _writer_role) => {
                    let opened = single_file.open().await?;
                    let indexer = opened.indexer;
                    let crdt_conn = opened
                        .crdt_conn
                        .expect("SingleFileStore::open always returns a CRDT connection");

                    // CRDT backend over a SECOND CONNECTION TO THE SAME INSTANCE
                    // (cloned inside `open()` before the write connection moved
                    // into the indexer) — not a second `Connection::open`,
                    // which would be a separate instance that clobbers chat
                    // writes on checkpoint.
                    //
                    // Kept as the CONCRETE type (not yet `Arc<dyn
                    // CrdtBackend>`) until after the `is_pg_catalog` branch
                    // below — `attach_shared_pg` (DuckLake PR 10) is only on
                    // `DuckdbCrdtBackend` itself, not the trait, and needs to
                    // run before the backend is handed out.
                    let crdt_backend_concrete =
                        DuckdbCrdtBackend::new(Arc::new(Mutex::new(crdt_conn)));

                    let mut publish_task_handle = None;
                    if backend == IndexBackend::DuckLake {
                        // Writer: attach the lake idempotently on the
                        // indexer's own connection (`ATTACH IF NOT EXISTS`)
                        // so a later publish never pays a fresh attach.
                        // Fail-closed: a broken lake config fails the boot,
                        // same posture as the reader's synchronous adopt.
                        let lake_cfg = self.lake.as_ref().expect(
                            "ESCUREL_INDEX_BACKEND=ducklake always carries a LakeConfig \
                         (EscurelConfig::from_env validated this)",
                        );
                        indexer.attach_lake(lake_cfg).await?;

                        // Phase B (DuckLake PR 8): the writer ALSO moves
                        // onto the shared chat Postgres table — chat
                        // re-homing is symmetric across every replica, not
                        // reader-only — reusing the same catalog_dsn (see
                        // the reader arm above for the `is_pg_catalog`
                        // rationale).
                        // Phase B (DuckLake PR 9): the writer ALSO moves
                        // onto the shared events Postgres table — events
                        // re-homing is symmetric across every replica,
                        // same as chat above.
                        // Phase B (DuckLake PR 10): the writer ALSO moves
                        // its CRDT op-log/snapshots onto the shared
                        // Postgres tables — TWO attaches (see the field doc
                        // on `Indexer::crdt_pg_backend`): one on the
                        // indexer's own connection (so `list_snapshots`
                        // reads the shared table), one on
                        // `crdt_backend_concrete`'s own connection (so the
                        // live-session actor path does).
                        if lake_cfg.is_pg_catalog() {
                            indexer.attach_chat_pg(&lake_cfg.catalog_dsn).await?;
                            indexer.attach_events_pg(&lake_cfg.catalog_dsn).await?;
                            indexer.attach_crdt_pg(&lake_cfg.catalog_dsn).await?;
                            crdt_backend_concrete
                                .attach_shared_pg(&lake_cfg.catalog_dsn, &self.tenant)
                                .await
                                .map_err(ConfigError::CrdtPg)?;
                        }

                        // Optional periodic publish (PR 7):
                        // `ESCUREL_SNAPSHOT_PUBLISH_SECS > 0`. `0` (default)
                        // keeps publishing manual-only via the
                        // `publish_snapshot` admin tool.
                        if self.snapshot_publish_secs > 0 {
                            let handle = IndexerHandle::fixed(Arc::clone(&indexer));
                            publish_task_handle = Some(
                                crate::snapshot_publish::PublishTask::new(
                                    handle,
                                    lake_cfg.clone(),
                                    Duration::from_secs(self.snapshot_publish_secs),
                                    self.snapshot_keep,
                                    Arc::clone(&last_published_epoch),
                                )
                                .spawn(),
                            );
                        }
                    }

                    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(crdt_backend_concrete);
                    (
                        IndexerHandle::fixed(indexer),
                        Some(crdt_backend),
                        false,
                        None,
                        publish_task_handle,
                    )
                }
            };

        // 5. OIDC verifier (only when an issuer is configured).
        let verifier = self.build_verifier();

        // 6. Quota + tenant store. Per-tenant overrides (#247) come from the
        //    served tenant's spec; absent → gateway defaults.
        let quota_mgr = QuotaManager::new(QuotaConfig::defaults());
        if let Some(over) = tenant_spec.as_ref().and_then(|s| s.quotas) {
            quota_mgr.set_for_tenant(
                &self.tenant,
                apply_quota_override(QuotaConfig::defaults(), over),
            );
        }
        let quota = Some(Arc::new(quota_mgr));
        let tenant_store = Arc::new(FsTenantStore::new(self.data_dir.join("tenants")));
        // #247: cache the served tenant's suspend flag for the dispatch gate.
        let tenant_suspended = Arc::new(std::sync::atomic::AtomicBool::new(matches!(
            tenant_spec.as_ref().map(|s| s.status),
            Some(escurel_admin::TenantStatus::Suspended)
        )));

        // 7. Readiness probe over the live dependencies.
        let readiness = Arc::new(DependencyProbe::new(
            Arc::clone(&store),
            Arc::clone(&embedder),
            self.tenant.clone(),
        ));

        let server_config = ServerConfig {
            // Per-instance write ACL (`ESCUREL_WRITE_ACL`): off (default) |
            // log | enforce. Read straight from env so it can be flipped at
            // deploy without a config-file change (safe dark→log→enforce rollout).
            write_acl: crate::WriteAclMode::from_env(),
            listen: self.listen_http.clone(),
            version: self.version.clone(),
            readiness,
            // The hard tenant boundary is driven by the configured tenant,
            // independent of the indexer, so it holds for every route.
            served_tenant: Some(self.tenant.clone()),
            indexer: Some(indexer_handle),
            verifier,
            quota,
            tenant_store: Some(tenant_store),
            crdt_backend,
            // A ducklake reader has no local write surface: no CRDT/chat,
            // no per-instance page edits, no event bus (Phase B — re-
            // homing those off the writer — is out of scope for this
            // PR). `dispatch_tools_call` consults this to reject the
            // mutating / chat-and-CRDT tool surface with a typed error
            // instead of silently misbehaving against an absent backend.
            reader_mode,
            // DuckLake publish/GC surface (PR 7): `None` for the
            // single-file backend, so `publish_snapshot` refuses with a
            // typed precondition error rather than running against a
            // backend that never publishes.
            lake: self.lake.clone(),
            snapshot_keep: self.snapshot_keep,
            last_published_epoch: Arc::clone(&last_published_epoch),
            // Hot-reload seam: the live embedder plus a factory that
            // rebuilds it from this config on demand. The
            // `embedding_reload` admin RPC retries a degraded-start
            // model load by calling the factory and swapping the
            // result into `embedder`.
            embedder_reload: Some(Arc::clone(&embedder)),
            embedder_factory: Some(embed_cfg.embedder_factory()),
            tenant_suspended,
            // #246 eager per-edit improvement: opt-in via env, off by default.
            emit_edit_events: std::env::var("ESCUREL_EMIT_EDIT_EVENTS")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            demo_dir: self.demo_dir.clone(),
            webhook_url: self.webhook_url.clone(),
            webhook_secret: self.webhook_secret.clone(),
            pack_secret: self.pack_secret.clone(),
            metrics_listen: self.metrics_listen.clone(),
        };

        let handle = serve(server_config)
            .await
            .map_err(|e| ConfigError::InvalidValue {
                var: "ESCUREL_SERVER_LISTEN_HTTP",
                value: e.to_string(),
                reason: "failed to bind / serve",
            })?;

        Ok(BootedServer {
            handle,
            embedder,
            refresh_handle,
            publish_handle,
        })
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

    /// Load the served tenant's spec from
    /// `<data_dir>/tenants/<tenant>/tenant.json` (#247). `None` when the tenant
    /// isn't provisioned or the file is absent/malformed — the gateway then
    /// runs on env defaults, exactly as before this field existed.
    async fn load_served_tenant_spec(&self) -> Option<escurel_admin::TenantSpec> {
        let path = self
            .data_dir
            .join("tenants")
            .join(&self.tenant)
            .join("tenant.json");
        let bytes = tokio::fs::read(&path).await.ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// A config clone whose embedding provider/model/dim are overridden by the
    /// tenant's declared `embedding_provider` (#247). No override → a plain
    /// clone (env defaults win).
    fn with_tenant_embedding(&self, spec: Option<&escurel_admin::TenantSpec>) -> Self {
        let mut cfg = self.clone();
        if let Some(ep) = spec.and_then(|s| s.embedding_provider.as_ref()) {
            if let Some(p) = parse_embedding_provider(&ep.provider) {
                cfg.embedding_provider = p;
            }
            if let Some(m) = ep.model.clone() {
                cfg.embedding_model = Some(m);
            }
            if let Some(d) = ep.dim {
                cfg.embedding_dim = d;
            }
        }
        cfg
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
        // Gemini is the default provider, but a key is not always present
        // (keyless dev, CI, air-gapped). Rather than fail the boot, fall back
        // to a ZeroEmbedder of the configured dimension and log a warning — the
        // server stays healthy (lexical search works); semantic search is inert
        // until a key is set + `embedding_reload` is called.
        let Some(key) = self.gemini_api_key.clone().filter(|k| !k.is_empty()) else {
            tracing::warn!(
                "ESCUREL_EMBEDDING_PROVIDER=gemini but ESCUREL_GEMINI_API_KEY is unset; \
                 falling back to zero-vector embeddings (semantic search disabled). Set a key \
                 (or ESCUREL_EMBEDDING_PROVIDER=embeddinggemma/zero) to silence this."
            );
            return Ok(Arc::new(ZeroEmbedder::new(self.embedding_dim)));
        };
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

    /// Attach the configured reranker to a freshly built indexer. `Off` (or a
    /// load failure) leaves the indexer reranker-less; `Bge` loads the
    /// cross-encoder and wires the rerank stage. Never fatal — a model load
    /// failure degrades to first-stage-only ranking with a warning.
    async fn attach_retrieval(&self, base: Indexer) -> Indexer {
        // Load the cross-encoder when rerank is on; a load failure is
        // degraded-start (warn + run without rerank), never fatal.
        let reranker = if self.rerank_mode == RerankMode::Bge {
            match self.load_reranker().await {
                Ok(r) => {
                    tracing::info!(
                        model = %self.rerank_model,
                        candidates = self.rerank_candidates,
                        "cross-encoder rerank enabled",
                    );
                    Some(r)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        model = %self.rerank_model,
                        "reranker failed to load; serving first-stage ranking (rerank disabled)",
                    );
                    None
                }
            }
        } else {
            None
        };

        // One RetrievalConfig carries both stages: rerank is enabled only if a
        // reranker actually loaded; two-pass is independent (issue #218).
        let mut retrieval = if reranker.is_some() {
            escurel_index::RetrievalConfig::enabled(self.rerank_candidates)
        } else {
            escurel_index::RetrievalConfig::disabled()
        };
        if self.two_pass {
            tracing::info!(
                coarse_dim = self.coarse_dim,
                coarse_candidates = self.coarse_candidates,
                "matryoshka two-pass vector search enabled",
            );
            retrieval = retrieval.with_two_pass(self.coarse_dim, self.coarse_candidates);
        }

        match reranker {
            Some(r) => base.with_reranker(r, retrieval),
            None => base.with_retrieval(retrieval),
        }
    }

    /// Load the candle cross-encoder reranker. The `rerank_model` is a local
    /// directory (air-gapped bake) when it exists on disk, else an HF repo id
    /// fetched into the hub cache on first boot.
    #[cfg(feature = "rerank")]
    async fn load_reranker(&self) -> Result<Arc<dyn escurel_embed::Reranker>, ConfigError> {
        use escurel_embed::CrossEncoderReranker;
        let model = &self.rerank_model;
        let dir = std::path::Path::new(model);
        let loaded = if dir.is_dir() {
            CrossEncoderReranker::from_local(
                &dir.join("config.json"),
                &dir.join("tokenizer.json"),
                &dir.join("model.safetensors"),
                model,
            )
        } else {
            CrossEncoderReranker::from_hf_hub(model).await
        }
        .map_err(|e| ConfigError::InvalidValue {
            var: "ESCUREL_RETRIEVAL_RERANK_MODEL",
            value: e.to_string(),
            reason: "failed to load the cross-encoder reranker",
        })?;
        Ok(Arc::new(loaded))
    }

    #[cfg(not(feature = "rerank"))]
    async fn load_reranker(&self) -> Result<Arc<dyn escurel_embed::Reranker>, ConfigError> {
        Err(ConfigError::EmbedderFeatureDisabled {
            provider: "rerank",
            feature: "rerank",
        })
    }

    fn build_verifier(&self) -> Option<Arc<OidcVerifier>> {
        let auth = self.auth.as_ref()?;
        let mut cfg = OidcConfig::new(auth.issuer.clone(), auth.audience.clone())
            .with_tenant_claim(auth.tenant_claim.clone())
            .with_admin_role(auth.admin_role_claim.clone(), auth.admin_role_value.clone())
            .with_groups_claim(auth.groups_claim.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg_from(vars: &[(&str, &str)]) -> EscurelConfig {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        EscurelConfig::from_source(&|k: &str| map.get(k).cloned()).unwrap()
    }

    #[test]
    fn ingest_contextualize_defaults_to_structural() {
        let cfg = cfg_from(&[]);
        assert_eq!(cfg.ingest_contextualize, ContextualizeMode::Structural);
    }

    #[test]
    fn ingest_contextualize_env_override_off() {
        let cfg = cfg_from(&[("ESCUREL_INGEST_CONTEXTUALIZE", "off")]);
        assert_eq!(cfg.ingest_contextualize, ContextualizeMode::Off);
    }
}

#[cfg(test)]
mod rerank_config_tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg_from(pairs: &[(&str, &str)]) -> Result<EscurelConfig, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        EscurelConfig::from_source(&move |k: &str| map.get(k).cloned())
    }

    #[test]
    fn rerank_default_matches_build_feature_and_knobs() {
        let c = cfg_from(&[]).expect("config builds");
        // "default-on where built": Bge under `--features rerank`, else Off.
        let expected = if cfg!(feature = "rerank") {
            RerankMode::Bge
        } else {
            RerankMode::Off
        };
        assert_eq!(c.rerank_mode, expected);
        assert_eq!(c.rerank_candidates, 100);
        assert_eq!(c.rerank_model, "BAAI/bge-reranker-v2-m3");
        assert_eq!(c.rerank_device, "cpu");
    }

    #[test]
    fn rerank_env_overrides_take_effect() {
        let c = cfg_from(&[
            ("ESCUREL_RETRIEVAL_RERANK", "bge"),
            ("ESCUREL_RETRIEVAL_RERANK_CANDIDATES", "50"),
            ("ESCUREL_RETRIEVAL_RERANK_MODEL", "BAAI/bge-reranker-base"),
        ])
        .expect("config builds");
        assert_eq!(c.rerank_mode, RerankMode::Bge);
        assert_eq!(c.rerank_candidates, 50);
        assert_eq!(c.rerank_model, "BAAI/bge-reranker-base");
    }

    #[test]
    fn rerank_explicit_off() {
        let c = cfg_from(&[("ESCUREL_RETRIEVAL_RERANK", "off")]).expect("config builds");
        assert_eq!(c.rerank_mode, RerankMode::Off);
    }

    #[test]
    fn rerank_invalid_value_errors() {
        let err = cfg_from(&[("ESCUREL_RETRIEVAL_RERANK", "bogus")]).expect_err("must reject");
        assert!(matches!(
            err,
            ConfigError::InvalidValue {
                var: "ESCUREL_RETRIEVAL_RERANK",
                ..
            }
        ));
    }

    #[test]
    fn two_pass_defaults_off_with_standard_knobs() {
        let c = cfg_from(&[]).expect("config builds");
        assert!(!c.two_pass);
        assert_eq!(c.coarse_dim, 128);
        assert_eq!(c.coarse_candidates, 500);
    }

    #[test]
    fn two_pass_env_overrides_take_effect() {
        let c = cfg_from(&[
            ("ESCUREL_RETRIEVAL_TWO_PASS", "true"),
            ("ESCUREL_RETRIEVAL_COARSE_DIM", "256"),
            ("ESCUREL_RETRIEVAL_COARSE_CANDIDATES", "800"),
        ])
        .expect("config builds");
        assert!(c.two_pass);
        assert_eq!(c.coarse_dim, 256);
        assert_eq!(c.coarse_candidates, 800);
    }

    #[test]
    fn two_pass_truthy_and_falsy_values() {
        for v in ["1", "yes", "on", "TRUE"] {
            assert!(
                cfg_from(&[("ESCUREL_RETRIEVAL_TWO_PASS", v)])
                    .unwrap()
                    .two_pass,
                "{v:?} should enable two-pass",
            );
        }
        for v in ["false", "0", "off", ""] {
            assert!(
                !cfg_from(&[("ESCUREL_RETRIEVAL_TWO_PASS", v)])
                    .unwrap()
                    .two_pass,
                "{v:?} should leave two-pass off",
            );
        }
    }

    #[test]
    fn coarse_dim_invalid_value_errors() {
        let err = cfg_from(&[("ESCUREL_RETRIEVAL_COARSE_DIM", "lots")]).expect_err("must reject");
        assert!(matches!(
            err,
            ConfigError::InvalidValue {
                var: "ESCUREL_RETRIEVAL_COARSE_DIM",
                ..
            }
        ));
    }
}
