# Discovered: quack + quack_oauth feasibility on DuckDB v1.5.5 (2026-09-12)

Feasibility gate for the Phase-4/6 scoped-Quack data plane (crew F3 + owner ask).
Run against `/usr/bin/duckdb` **v1.5.5 (Variegata) d8cdaa33fd** — escurel's exact
pinned version (duckdb crate 1.10505.0).

## Results

1. **Extensions load on v1.5.5.** `quack` is an OFFICIAL DuckDB extension
   (installed from `extensions.duckdb.org`, not a community/version-gap risk like
   duckpgq on 1.5.3). `quack_oauth` installs from `http://get.erpl.io` (reachable,
   http 200). Both `LOAD` with `-unsigned`; `duckdb_extensions()` reports
   `loaded=true, installed=true` for both. Escurel already runs
   `allow_unsigned_extensions` for sql_view, so serving them adds no NEW
   process-hardening weakening.

2. **quack_oauth config surface executes cleanly on v1.5.5** (no IdP needed for
   this part): `SET quack_authentication_function='quack_oauth_check_token'` +
   `SET quack_authorization_function='quack_oauth_check_authorization'`;
   `CREATE SECRET (TYPE quack_oauth_server, issuer/jwks_uri/audience, policy_table,
   audit_table)`; the 7-column policy table (priority, subject, any_scope[],
   actions[], object_pattern, column_pattern, allow) with default-deny + two
   scoped-allow rows (`Scan` on `main.entitled_*`, `Insert` on `result.rows`); and
   the audit table — all succeed.

3. **Authorization model = object/column/action ABAC, default-deny** (README +
   src/policy*.cpp + test/cpp/test_policy.cpp). The action is parsed by DuckDB's
   OWN parser (Scan/Insert/Update/Delete/Ddl/CopyTo/CopyFrom/Attach/Pragma/
   ServeAdmin); objects+columns are walked recursively; first-match-wins;
   unknown action names fail the policy closed. This is the enforcement layer the
   crew (F1/F4) said DuckDB lacked — it is at the quack_oauth layer, not DuckDB
   grants.

## Still to prove (build-time, with a live token)

The adversarial ENFORCEMENT matrix (a real authenticated session must be DENIED
SET/ATTACH/COPY TO/duckdb_secrets()/pg_catalog/postgres_query/read_csv(http)/…,
and ALLOWED only the entitled view + the one result write) needs a live IdP
token. The repo SHIPS this: `test/integration/keycloak/` (docker-compose +
`oauth_policy_table_keycloak.test.template`). Reuse that harness as escurel's
first integration test rather than re-deriving it. Also verify there: whether
system-table filtering (`duckdb_secrets()`, `pg_catalog.*`, `duckdb_*` are
filtered from the parsed object list) means those reads are DENIED (good) or
UNGATED (must be closed by an explicit deny rule / a default-deny on Scan with no
object match).

## Verdict

The two gating unknowns (extension availability for v1.5.5; whether an
enforceable per-session boundary exists at all) are RESOLVED in favour of the
Quack + quack_oauth direction. The design is de-risked enough to commit: write
rev.3 around it and build, with the Keycloak enforcement matrix as the security
gate before the seam ships.
