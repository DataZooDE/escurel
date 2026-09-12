//! quack + quack_oauth go/no-go spike (fleet #801 Phase 4 — the scoped-Quack
//! data plane).
//!
//! `#[ignore]` by default — it reaches the DuckDB extension registries over the
//! network and loads UNSIGNED extensions (`quack` from the official repo,
//! `quack_oauth` from `http://get.erpl.io`), neither of which belongs in the
//! normal `cargo test` gate. Run it explicitly:
//!
//! ```sh
//! cargo test -p escurel-index --test suite -- --ignored --nocapture quack_load_probe
//! ```
//!
//! This proves the fact that decides whether Phase 4 slice 3b's no-mock
//! enforcement-matrix test is buildable **inside escurel's own process** (the
//! bundled libduckdb the crate links), rather than only the CLI:
//!
//! GO  = `INSTALL quack; LOAD quack; INSTALL quack_oauth FROM 'http://get.erpl.io';
//!        LOAD quack_oauth;` all succeed on the pinned libduckdb, AND the
//!        quack_oauth authorization surface configures cleanly (callback swap +
//!        a default-deny object/action SQL policy table + audit table).
//! NO-GO = any of those fails (most likely: no prebuilt binary for this exact
//!        libduckdb, or the crate cannot fetch/load an unsigned extension).
//!
//! The full ENFORCEMENT matrix (a live-token session denied SET/ATTACH/COPY/
//! catalog/secret reads, allowed only the entitled view) is slice 3b's green,
//! gated on the design sign-off; this probe only settles loadability + config.
//! Outcome recorded under docs/notes/discovered/.

use duckdb::{Config, Connection};
use tempfile::TempDir;

#[test]
#[ignore = "network + unsigned extensions; run explicitly for the quack/quack_oauth go/no-go"]
fn quack_and_quack_oauth_load_and_configure_in_process() {
    let dir = TempDir::new().expect("tempdir");
    // allow_unsigned_extensions is an open-time Config flag (the same one
    // Migrator::open_config sets from ESCUREL_ALLOW_UNSIGNED_EXTENSIONS) — it
    // cannot be SET once the database is running.
    let config = Config::default()
        .allow_unsigned_extensions()
        .expect("config");
    let conn = Connection::open_with_flags(dir.path().join("probe.duckdb"), config).expect("open");

    // 1. Load both extensions in-process (quack = official repo; quack_oauth =
    //    the DataZoo distribution bucket).
    if let Err(e) = conn.execute_batch(
        "INSTALL quack; LOAD quack; \
         INSTALL quack_oauth FROM 'http://get.erpl.io'; LOAD quack_oauth;",
    ) {
        println!(
            "NO-GO: loading quack/quack_oauth failed in-process on this libduckdb: {e}\n\
             Slice 3b's no-mock enforcement matrix would need vendored extension \
             binaries or a different harness."
        );
        return;
    }

    let loaded: i64 = conn
        .query_row(
            "SELECT count(*) FROM duckdb_extensions() \
             WHERE extension_name IN ('quack','quack_oauth') AND loaded",
            [],
            |r| r.get(0),
        )
        .expect("query duckdb_extensions");
    assert_eq!(loaded, 2, "both quack and quack_oauth must report loaded");

    // 2. The quack_oauth AUTHORIZATION surface configures cleanly: swap in the
    //    real callbacks, declare a resource-server SECRET with a policy table,
    //    and a DEFAULT-DENY object/action policy (allow Scan only on the
    //    entitled views, Insert only on the result namespace).
    conn.execute_batch(
        "SET quack_authentication_function = 'quack_oauth_check_token'; \
         SET quack_authorization_function  = 'quack_oauth_check_authorization'; \
         CREATE SECRET rs (TYPE quack_oauth_server, issuer 'https://idp.example/', \
            jwks_uri 'https://idp.example/jwks', audience 'escurel-quack', \
            policy_table 'main.policies', audit_table 'main.audit'); \
         SET quack_oauth_provider = 'generic'; \
         SET quack_oauth_server_secret_name = 'rs'; \
         CREATE TABLE main.policies (priority INTEGER NOT NULL, subject VARCHAR, \
            any_scope VARCHAR[], actions VARCHAR[], object_pattern VARCHAR, \
            column_pattern VARCHAR, allow BOOLEAN NOT NULL); \
         INSERT INTO main.policies VALUES \
            (10, NULL, ['scenario:run'], ['Scan'],   'main.entitled_*', NULL, true), \
            (20, NULL, ['scenario:run'], ['Insert'], 'result.rows',     NULL, true); \
         CREATE TABLE main.audit (timestamp_unix_s BIGINT, event_type VARCHAR, \
            subject VARCHAR, issuer VARCHAR, kid VARCHAR, token_hash VARCHAR, \
            action VARCHAR, reason VARCHAR);",
    )
    .expect("NO-GO: quack_oauth authorization config surface failed to apply");

    let rules: i64 = conn
        .query_row("SELECT count(*) FROM main.policies", [], |r| r.get(0))
        .expect("policy rows");
    assert_eq!(rules, 2, "the default-deny + scoped-allow policy loaded");

    println!(
        "GO: quack + quack_oauth load in-process on the pinned libduckdb, and the \
         quack_oauth object/action policy surface configures cleanly. Slice 3b's \
         no-mock enforcement matrix (live-token adversarial cases) is buildable in \
         escurel's own test process."
    );
}
