//! Live-CRDT test helpers for downstream consumers.
//!
//! The three session tools (`open_session` / `apply_op` /
//! `close_session`) only function when the gateway is spawned with a
//! [`CrdtBackend`] wired. Standing one up means an in-schema DuckDB
//! connection + a `DuckdbCrdtBackend`, and exercising `apply_op` means
//! producing a real base64 Loro op blob. Both are fiddly and pull in
//! `escurel-crdt`, `duckdb`, and `loro` — deps a consumer's CLI/app
//! crate should not have to carry. This module hides them behind two
//! declarative helpers so a consumer test reads:
//!
//! ```no_run
//! use escurel_test_support::{ConfigOverrides, EscurelProcess, Opts};
//! use escurel_test_support::crdt_testkit::{duckdb_crdt_backend, loro_insert_op};
//!
//! # async fn run() {
//! let process = EscurelProcess::spawn(Opts {
//!     config_overrides: ConfigOverrides {
//!         crdt_backend: Some(duckdb_crdt_backend()),
//!         disable_indexer: true,
//!         ..Default::default()
//!     },
//!     ..Default::default()
//! }).await;
//! let _op = loro_insert_op("hello");
//! # }
//! ```

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use loro::{ExportMode, LoroDoc, VersionVector};
use tokio::sync::Mutex;

/// Build a live [`CrdtBackend`] over a fresh in-memory DuckDB with the
/// v1 schema applied — ready to wire into
/// [`ConfigOverrides::crdt_backend`](crate::ConfigOverrides). The backend
/// owns the connection for its lifetime, so the returned handle is the
/// only thing the caller needs to keep alive.
#[must_use]
pub fn duckdb_crdt_backend() -> Arc<dyn CrdtBackend> {
    let conn = Connection::open_in_memory().expect("open in-memory duckdb");
    Migrator::up(&conn).expect("duckdb migrations");
    Arc::new(DuckdbCrdtBackend::new(Arc::new(Mutex::new(conn))))
}

/// Mint a base64-encoded Loro op blob that inserts `text` at the head of
/// a document's `body` text container — the exact wire shape
/// `apply_op` / `session apply` expect. The op carries the full history
/// of a fresh peer, so it imports cleanly into an empty session doc.
#[must_use]
pub fn loro_insert_op(text: &str) -> String {
    let doc = LoroDoc::new();
    doc.get_text("body")
        .insert(0, text)
        .expect("loro text insert");
    doc.commit();
    let bytes = doc
        .export(ExportMode::updates(&VersionVector::default()))
        .expect("loro export updates");
    B64.encode(bytes)
}
