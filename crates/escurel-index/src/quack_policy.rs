//! Building a `quack_oauth` per-session policy for a delegated data-plane
//! session (async-ops Phase 4, Path A / Half 2).
//!
//! When escurel serves a delegated agent a scoped Quack session, the session's
//! authority is exactly the rows this module emits into quack_oauth's policy
//! table. The rev.5 crew security review pinned the shape these rows MUST have
//! (findings F3 + F5):
//!
//! - **Subject-bound, not scope-bound (F3).** A policy table is shared by every
//!   session, so a row keyed only by a scope (`any_scope`) with `subject NULL`
//!   matches ANY token carrying that scope — including another tenant's. Every
//!   row here is bound to the verified requester's `subject`, so it authorizes
//!   only that requester's session.
//! - **Literal object matching, no globs (F3).** quack_oauth matches
//!   `object_pattern` with a `*`-glob; a pattern like `result_acme.*` would also
//!   match `result_acme_evil.x`, and a tenant id containing a `*` would widen
//!   the grant. Every object here is a validated, fully-qualified
//!   `schema.table` with no glob metacharacters, so the match is exact.
//! - **One quarantine result table, not a namespace (F5).** The session may
//!   `Insert` into exactly ONE server-named result table
//!   (`<result_schema>.<run table>`), never a whole `result_*` schema, so a
//!   delegated run cannot touch another run's result.
//!
//! Default-deny is quack_oauth's `quack_oauth_policy_default = 'deny'` setting,
//! so this module emits ONLY allow rows — the entitled reads + the one result
//! write. Everything else (table functions, other schemas, DDL, Attach, Pragma,
//! CopyTo) is denied by the absence of a matching allow row.
//!
//! This is the pure policy-row builder. Installing the rows (writing them into
//! the session's policy table) and serving the session are the gateway runtime,
//! kept separate so this — the security-load-bearing shape — is unit-testable
//! without a Quack server.

/// A single row of quack_oauth's 7-column policy table.
///
/// Column order matches the table schema
/// `(priority, subject, any_scope, actions, object_pattern, column_pattern, allow)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRow {
    /// Lower fires first; ties resolve by table order.
    pub priority: i32,
    /// The verified requester this row authorizes. Always `Some` here — a
    /// `None` (SQL `NULL`) row would match any subject, the F3 hole.
    pub subject: Option<String>,
    /// Scope constraint. Empty = "no scope constraint" — we bind by `subject`
    /// instead, so this stays empty.
    pub any_scope: Vec<String>,
    /// The quack_oauth actions this row allows (`Scan`, `Insert`, …).
    pub actions: Vec<String>,
    /// The fully-qualified `schema.table` this row targets — a validated
    /// literal, never a glob.
    pub object_pattern: Option<String>,
    /// Column constraint; `None` = all columns.
    pub column_pattern: Option<String>,
    /// Allow (true) — this builder never emits deny rows (default-deny covers
    /// the rest).
    pub allow: bool,
}

/// Why a scoped policy could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// The requester subject was empty — a policy row with no subject is the
    /// F3 hole (matches any token), so refuse rather than emit it.
    #[error("scoped policy: requester subject must not be empty")]
    EmptySubject,
    /// An object was not a validated, glob-free `schema.table`.
    #[error("scoped policy: {0:?} is not a bounded `schema.table` object")]
    InvalidObject(String),
    /// No entitled objects were supplied — a read-nothing session is almost
    /// certainly a mistake, and an empty allow-set is safer surfaced than
    /// silently shipped.
    #[error("scoped policy: at least one entitled object is required")]
    NoEntitledObjects,
}

/// Priority band for the generated allow rows (well below any operator override).
const SCOPED_PRIORITY: i32 = 100;

/// A fully-qualified object is exactly `<segment>.<segment>`, each segment
/// 1-128 chars of `[A-Za-z0-9_-]`. The single `.` is the schema/table
/// separator; NO other `.`, and none of `*?[]`, `/`, `:`, quotes or spaces —
/// so the value is a safe glob-free literal AND cannot carry a path, scheme, or
/// SQL-quote break when spliced into the policy table.
fn is_qualified_object(s: &str) -> bool {
    let Some((schema, table)) = s.split_once('.') else {
        return false;
    };
    let ok_segment = |seg: &str| {
        !seg.is_empty()
            && seg.len() <= 128
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    ok_segment(schema) && ok_segment(table)
}

/// Build the per-session allow rows for a delegated data-plane session: the
/// requester may `Scan` each entitled object and `Insert` into the one
/// server-named result table, and nothing else (default-deny).
///
/// # Errors
/// [`PolicyError`] when the subject is empty, no entitled objects are given, or
/// any object (entitled or result) is not a bounded glob-free `schema.table`.
pub fn scoped_session_policy(
    requester_subject: &str,
    entitled_objects: &[String],
    result_table: &str,
) -> Result<Vec<PolicyRow>, PolicyError> {
    if requester_subject.is_empty() {
        return Err(PolicyError::EmptySubject);
    }
    if entitled_objects.is_empty() {
        return Err(PolicyError::NoEntitledObjects);
    }
    if !is_qualified_object(result_table) {
        return Err(PolicyError::InvalidObject(result_table.to_owned()));
    }
    for obj in entitled_objects {
        if !is_qualified_object(obj) {
            return Err(PolicyError::InvalidObject(obj.clone()));
        }
    }

    let subject = Some(requester_subject.to_owned());
    let mut rows = Vec::with_capacity(entitled_objects.len() + 1);
    // One Scan-allow per entitled object — subject-bound, literal object.
    for obj in entitled_objects {
        rows.push(PolicyRow {
            priority: SCOPED_PRIORITY,
            subject: subject.clone(),
            any_scope: Vec::new(),
            actions: vec!["Scan".to_owned()],
            object_pattern: Some(obj.clone()),
            column_pattern: None,
            allow: true,
        });
    }
    // Exactly one Insert-allow, into the single quarantine result table (F5).
    rows.push(PolicyRow {
        priority: SCOPED_PRIORITY,
        subject,
        any_scope: Vec::new(),
        actions: vec!["Insert".to_owned()],
        object_pattern: Some(result_table.to_owned()),
        column_pattern: None,
        allow: true,
    });
    Ok(rows)
}

/// Single-quote-escape a value for a SQL string literal (double every `'`).
/// Object ids are already validated to `[A-Za-z0-9_.-]`, but `subject` and
/// scopes come from the token, so escape defensively — a delegation `sub` like
/// `google:o'brien@acme` must not break the INSERT.
fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Render a `VARCHAR[]` literal (`['a', 'b']`), each element escaped.
fn sql_str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| sql_str(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// Render one value that is either a SQL string literal or `NULL`.
fn sql_opt(s: &Option<String>) -> String {
    match s {
        Some(v) => sql_str(v),
        None => "NULL".to_owned(),
    }
}

/// Render the `INSERT` that installs `rows` into the quack_oauth policy table
/// `policy_table` (a validated `schema.table`), or `None` when there are no rows.
///
/// Column order matches the 7-column policy schema
/// `(priority, subject, any_scope, actions, object_pattern, column_pattern, allow)`.
/// Every string value is single-quote-escaped, so a token-derived `subject`
/// cannot break out of its literal; the policy table name is validated as a
/// bounded `schema.table` first (an invalid one yields `None` rather than an
/// unsafe splice).
#[must_use]
pub fn render_policy_inserts(rows: &[PolicyRow], policy_table: &str) -> Option<String> {
    if rows.is_empty() || !is_qualified_object(policy_table) {
        return None;
    }
    let values: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "({}, {}, {}, {}, {}, {}, {})",
                r.priority,
                sql_opt(&r.subject),
                sql_str_array(&r.any_scope),
                sql_str_array(&r.actions),
                sql_opt(&r.object_pattern),
                sql_opt(&r.column_pattern),
                r.allow,
            )
        })
        .collect();
    Some(format!(
        "INSERT INTO {policy_table} \
         (priority, subject, any_scope, actions, object_pattern, column_pattern, allow) VALUES {};",
        values.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn rows_are_subject_bound_literal_and_scope_the_reads_plus_one_write() {
        let rows = scoped_session_policy(
            "google:alice@acme",
            &objs(&["main.vw_orders", "main.vw_customers"]),
            "result_acme.res_01hx",
        )
        .expect("policy");

        // Two Scan rows + one Insert row.
        assert_eq!(rows.len(), 3);
        // Every row is bound to the requester (never NULL — the F3 hole) and
        // carries no scope constraint (we bind by subject, not scope).
        for r in &rows {
            assert_eq!(r.subject.as_deref(), Some("google:alice@acme"));
            assert!(r.any_scope.is_empty(), "subject-bound, not scope-bound");
            assert!(r.allow);
            // Object patterns are literal (no glob metacharacters).
            let p = r.object_pattern.as_deref().unwrap();
            assert!(!p.contains(['*', '?', '[']), "literal object, got {p:?}");
        }
        let scans: Vec<_> = rows
            .iter()
            .filter(|r| r.actions == ["Scan"])
            .filter_map(|r| r.object_pattern.clone())
            .collect();
        assert_eq!(scans, vec!["main.vw_orders", "main.vw_customers"]);
        let inserts: Vec<_> = rows
            .iter()
            .filter(|r| r.actions == ["Insert"])
            .filter_map(|r| r.object_pattern.clone())
            .collect();
        // Exactly ONE writable table — the quarantine result table (F5), not a
        // `result_acme.*` namespace.
        assert_eq!(inserts, vec!["result_acme.res_01hx"]);
    }

    #[test]
    fn an_empty_subject_is_refused_never_emitting_a_null_subject_row() {
        assert_eq!(
            scoped_session_policy("", &objs(&["main.v"]), "result_acme.res_1"),
            Err(PolicyError::EmptySubject)
        );
    }

    #[test]
    fn a_glob_traversal_or_scheme_object_is_rejected() {
        // A wildcard result namespace (the F5 hole), a traversal, a scheme, a
        // bare name, a quote-break, and a space all fail is_qualified_object
        // before any row is emitted — as an entitled object OR the result table.
        for bad in [
            "result_acme.*",
            "../etc.passwd",
            "s3:bucket.key",
            "notqualified",
            "main.a'; DROP TABLE x;--",
            "main.a b",
        ] {
            assert_eq!(
                scoped_session_policy("s", &objs(&[bad]), "result_acme.res_1"),
                Err(PolicyError::InvalidObject(bad.to_owned())),
                "object {bad:?} must be rejected"
            );
            assert_eq!(
                scoped_session_policy("s", &objs(&["main.v"]), bad),
                Err(PolicyError::InvalidObject(bad.to_owned())),
                "result table {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn a_plain_literal_object_is_accepted_no_prefix_collision_at_the_glob_level() {
        // `result_acme_evil.x` shares a prefix with `result_acme` but, because
        // we emit LITERAL object patterns (never `result_acme*`), it can only
        // ever match itself — there is no prefix-collision surface. It is a
        // valid object and accepted.
        assert!(
            scoped_session_policy("s", &objs(&["result_acme_evil.x"]), "result_acme.res_1").is_ok()
        );
    }

    #[test]
    fn no_entitled_objects_is_refused() {
        assert_eq!(
            scoped_session_policy("s", &[], "result_acme.res_1"),
            Err(PolicyError::NoEntitledObjects)
        );
    }

    #[test]
    fn rendered_inserts_round_trip_into_a_real_policy_table() {
        let rows = scoped_session_policy(
            "google:alice@acme",
            &objs(&["main.vw_orders"]),
            "result_acme.res_01hx",
        )
        .expect("policy");
        let sql = render_policy_inserts(&rows, "main.policies").expect("sql");

        let conn = duckdb::Connection::open_in_memory().expect("duckdb");
        conn.execute_batch(
            "CREATE TABLE main.policies (priority INTEGER NOT NULL, subject VARCHAR, \
             any_scope VARCHAR[], actions VARCHAR[], object_pattern VARCHAR, \
             column_pattern VARCHAR, allow BOOLEAN NOT NULL);",
        )
        .expect("create policies");
        // The rendered INSERT is valid, executable SQL.
        conn.execute_batch(&sql).expect("install policy");

        // Read the rows back and confirm they match what the builder produced:
        // subject-bound, correct action, literal object, allow.
        let mut stmt = conn
            .prepare(
                "SELECT subject, actions[1], object_pattern, allow \
                 FROM main.policies ORDER BY object_pattern",
            )
            .expect("prepare");
        let got: Vec<(String, String, String, bool)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "google:alice@acme".to_owned(),
                    "Scan".to_owned(),
                    "main.vw_orders".to_owned(),
                    true
                ),
                (
                    "google:alice@acme".to_owned(),
                    "Insert".to_owned(),
                    "result_acme.res_01hx".to_owned(),
                    true
                ),
            ]
        );
    }

    #[test]
    fn a_subject_with_a_quote_is_escaped_not_injected() {
        // A token `sub` carrying a single quote must be escaped into the literal,
        // never break out of it. If escaping were wrong, this INSERT would be a
        // syntax error or an injection; instead it stores the value verbatim.
        let rows =
            scoped_session_policy("google:o'brien@acme", &objs(&["main.v"]), "result_x.res_1")
                .expect("policy");
        let sql = render_policy_inserts(&rows, "main.policies").expect("sql");
        assert!(
            sql.contains("'google:o''brien@acme'"),
            "quote must be doubled: {sql}"
        );

        let conn = duckdb::Connection::open_in_memory().expect("duckdb");
        conn.execute_batch(
            "CREATE TABLE main.policies (priority INTEGER NOT NULL, subject VARCHAR, \
             any_scope VARCHAR[], actions VARCHAR[], object_pattern VARCHAR, \
             column_pattern VARCHAR, allow BOOLEAN NOT NULL);",
        )
        .expect("create");
        conn.execute_batch(&sql).expect("install");
        let subj: String = conn
            .query_row("SELECT subject FROM main.policies LIMIT 1", [], |r| {
                r.get(0)
            })
            .expect("read");
        assert_eq!(subj, "google:o'brien@acme");
    }

    #[test]
    fn render_is_none_for_empty_rows_or_an_invalid_policy_table() {
        assert!(render_policy_inserts(&[], "main.policies").is_none());
        let rows =
            scoped_session_policy("s", &objs(&["main.v"]), "result_x.res_1").expect("policy");
        // An unqualified / injectable policy-table name is refused (no unsafe splice).
        assert!(render_policy_inserts(&rows, "main.policies; DROP TABLE x").is_none());
        assert!(render_policy_inserts(&rows, "notqualified").is_none());
    }
}
