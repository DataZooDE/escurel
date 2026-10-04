//! The SQL rows backend and its write-back against a REAL Postgres (a container, started and stopped
//! here with the docker CLI on a fixed host port so it can be taken down and brought back mid-test).
//! Real DuckDB `postgres` extension, real libpq-protocol seeding/observation (`tokio-postgres`), real
//! gateway over `POST /mcp`, no mocks. Opt-in (needs Docker):
//! `cargo test -p escurel-server --features live-postgres --test suite sql_rows_postgres`.
//!
//! What only Postgres can show: typed columns (NUMERIC, TIMESTAMP), a trigger that COUNTS how often a
//! row was updated (so "applied once" is a fact about the database, not about our events), a source
//! that is stopped mid-promote (dead letter) and comes back (a later promote lands), and the egress
//! policy refusing a private database host before any connection is attempted.
#![cfg(feature = "live-postgres")]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use duckdb::Connection;
use escurel_auth::Role;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::egress::EgressPolicy;
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_postgres::NoTls;

use super::sql_rows_db::{call_as, raw_call};

const TENANT: &str = "acme";
const ROWS: usize = 2_500;

/// A Postgres container on a fixed host port; removed on drop.
struct Pg {
    id: String,
    port: u16,
}

impl Pg {
    fn docker(args: &[&str]) -> String {
        let out = Command::new("docker").args(args).output().expect("docker");
        assert!(
            out.status.success(),
            "docker {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    async fn start() -> Self {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let id = Self::docker(&[
            "run",
            "-d",
            "-p",
            &format!("127.0.0.1:{port}:5432"),
            "-e",
            "POSTGRES_PASSWORD=postgres",
            "postgres:16-alpine",
        ]);
        let pg = Self { id, port };
        pg.wait_ready().await;
        pg
    }

    fn dsn(&self) -> String {
        format!(
            "host=127.0.0.1 port={} user=postgres password=postgres dbname=postgres",
            self.port
        )
    }

    async fn wait_ready(&self) {
        for _ in 0..120 {
            if tokio_postgres::connect(&self.dsn(), NoTls).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("postgres did not become ready");
    }

    async fn client(&self) -> tokio_postgres::Client {
        let (c, conn) = tokio_postgres::connect(&self.dsn(), NoTls)
            .await
            .expect("connect");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        c
    }

    fn stop(&self) {
        Self::docker(&["stop", "-t", "1", &self.id]);
    }

    async fn restart(&self) {
        Self::docker(&["start", &self.id]);
        self.wait_ready().await;
    }
}

impl Drop for Pg {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.id]).output();
    }
}

fn doc(n: usize) -> String {
    format!("{:010}", 4_500_000 + n)
}
fn row_page(n: usize) -> String {
    format!("markdown/instances/pg-order/{}.md", doc(n))
}

const SCHEMA: &str = "
CREATE TABLE public.orders (
  vbeln   TEXT PRIMARY KEY,
  kunnr   TEXT,
  netwr   NUMERIC(12,2),
  discount NUMERIC(5,2),
  qty     INTEGER,
  created TIMESTAMP,
  status  TEXT
);
CREATE TABLE public.touch_log (vbeln TEXT, at TIMESTAMPTZ DEFAULT now());
CREATE FUNCTION public.log_touch() RETURNS trigger AS $$
BEGIN INSERT INTO public.touch_log(vbeln) VALUES (NEW.vbeln); RETURN NEW; END $$ LANGUAGE plpgsql;
CREATE TRIGGER orders_touch AFTER UPDATE ON public.orders FOR EACH ROW EXECUTE FUNCTION public.log_touch();
-- a table WITHOUT a primary key, with a NULL key row: it must not become an instance
CREATE TABLE public.loose (code TEXT, label TEXT);
";

async fn seed(c: &tokio_postgres::Client) {
    c.batch_execute(SCHEMA).await.expect("schema");
    c.batch_execute(&format!(
        "INSERT INTO public.orders
           SELECT to_char(4500000 + i, 'FM0000000000'), to_char(1000000 + i % 40, 'FM0000000'),
                  100.00 + i, 5.00, 10 + i % 7, timestamp '2026-09-01 08:00' + (i || ' minutes')::interval, 'open'
           FROM generate_series(0, {}) i;
         INSERT INTO public.loose VALUES ('a','A'),('b','B'),(NULL,'orphan');",
        ROWS - 1
    ))
    .await
    .expect("rows");
}

fn skill_page() -> String {
    r#"---
kind: skill
id: pg-order
description: An order of the shop's Postgres database, one instance per row.
fields:
  - {name: sales_doc, kind: string, required: true, label: "Sales document"}
  - {name: sold_to, kind: string, label: "Sold-to"}
  - {name: net_value, kind: float, label: "Net value"}
  - {name: discount, kind: float, label: "Discount %"}
  - {name: quantity, kind: int, label: "Quantity"}
  - {name: created, kind: datetime, label: "Created"}
  - {name: status, kind: string, label: "Status"}
backend:
  kind: sql_view
  instances: rows
  key: vbeln
  linked: markdown
  filterable: [kunnr]
  writable_columns: [status, quantity, discount]
  source: {connector: postgres, attach: shop_pg, relation: "public.orders"}
  project: {vbeln: sales_doc, kunnr: sold_to, netwr: net_value, discount: discount, qty: quantity, created: created, status: status}
---
# pg-order
"#
    .to_owned()
}

struct Gw {
    p: EscurelProcess,
    pg: Pg,
    _dirs: Vec<TempDir>,
}

fn policy(secret_dir: &std::path::Path) -> EgressPolicy {
    let mut p = EgressPolicy {
        allow_loopback: true,
        // Writes are retried with a short pause so the dead-letter test is quick.
        write_retry_backoff: Duration::from_millis(50),
        ..EgressPolicy::default()
    };
    p.secrets.file_dirs = vec![secret_dir.to_path_buf()];
    p
}

impl Gw {
    async fn start() -> Self {
        Self::start_with(None).await
    }

    async fn start_with(timeout: Option<Duration>) -> Self {
        let pg = Pg::start().await;
        seed(&pg.client().await).await;
        let store_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let secret_dir = TempDir::new().unwrap();
        let secret: PathBuf = secret_dir.path().join("shop-pg");
        std::fs::write(&secret, format!("{}\n", pg.dsn())).unwrap();
        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();
        let mut indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
        if let Some(t) = timeout {
            indexer = indexer.with_rows_query_timeout(t);
        }
        let indexer = Arc::new(indexer);
        let p = EscurelProcess::spawn(Opts {
            auth: AuthMode::TestIssuer,
            config_overrides: ConfigOverrides {
                indexer: Some(indexer),
                egress: Some(policy(secret_dir.path())),
                signing: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let g = Self {
            p,
            pg,
            _dirs: vec![store_dir, db_dir, secret_dir],
        };
        let reg = g
            .admin(
                "register_credential",
                json!({ "name": "shop_pg", "connector": "postgres",
                        "secret_ref": format!("file:{}", secret.display()) }),
            )
            .await;
        assert_eq!(reg["ok"], true, "{reg}");
        let r = g
            .admin(
                "update_page",
                json!({ "page_id": "markdown/skills/pg-order.md", "content": skill_page() }),
            )
            .await;
        assert_eq!(r["ok"], true, "{r}");
        g
    }

    async fn admin(&self, name: &str, args: Value) -> Value {
        call_as(&self.p, &self.p.mint_token(TENANT, Role::Admin), name, args).await
    }

    async fn etag(&self, n: usize) -> String {
        let r = self
            .admin("expand", json!({ "page_id": row_page(n) }))
            .await;
        r["backend_projection"]["etag"]
            .as_str()
            .unwrap_or_else(|| panic!("no etag: {r}"))
            .to_owned()
    }

    async fn draft(&self, n: usize, patch: &str, etag: &str) -> Value {
        let content = format!(
            "---\nkind: instance\nid: {}\nskill: pg-order\nwrite_back:\n  patch: {{ {patch} }}\n  base_etag: \"{etag}\"\n---\nn\n",
            doc(n)
        );
        self.admin(
            "create_draft",
            json!({ "target_page_id": row_page(n), "content": content }),
        )
        .await
    }

    async fn promote(&self, id: &str) -> Value {
        self.admin("promote_draft", json!({ "draft_id": id })).await
    }
}

async fn touches(db: &tokio_postgres::Client) -> i64 {
    db.query_one("SELECT count(*) FROM public.touch_log", &[])
        .await
        .unwrap()
        .get(0)
}

fn draft_id(v: &Value) -> String {
    v["draft"]["draft_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no draft: {v}"))
        .to_owned()
}

fn codes(v: &Value) -> Vec<String> {
    v["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn rows_over_real_postgres_page_exactly_once_with_typed_columns() {
    let g = Gw::start().await;
    let mut seen = 0usize;
    let mut cursor: Option<String> = None;
    let mut first: Option<Value> = None;
    loop {
        let r = g
            .admin(
                "list_instances",
                json!({ "skill": "pg-order", "limit": 500, "cursor": cursor }),
            )
            .await;
        for i in r["instances"].as_array().expect("instances") {
            if first.is_none() {
                first = Some(i.clone());
            }
            seen += 1;
        }
        match r["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, ROWS, "every row exactly once");
    let first = first.unwrap();
    assert!(
        first["frontmatter"]["net_value"].is_number(),
        "NUMERIC is a number: {first}"
    );
    assert!(first["frontmatter"]["quantity"].is_number(), "{first}");
    assert!(
        first["frontmatter"]["created"]
            .as_str()
            .is_some_and(|s| s.starts_with("2026-09-01")),
        "TIMESTAMP is an ISO string: {first}"
    );
    // The filterable column narrows with a bound parameter.
    let r = g
        .admin(
            "list_instances",
            json!({ "skill": "pg-order", "limit": 10_000,
                    "frontmatter_key": "sold_to", "frontmatter_value": "1000007" }),
        )
        .await;
    assert_eq!(
        r["instances"].as_array().unwrap().len(),
        (0..ROWS).filter(|i| i % 40 == 7).count()
    );
}

#[tokio::test]
async fn a_promoted_write_back_updates_one_row_once_as_the_databases_own_trigger_sees_it() {
    let g = Gw::start().await;
    let db = g.pg.client().await;
    let e = g.etag(7).await;
    let id = draft_id(
        &g.draft(7, "status: shipped, quantity: 12, discount: 12.5", &e)
            .await,
    );
    assert_eq!(touches(&db).await, 0, "creating a draft touches nothing");

    let done = g.promote(&id).await;

    assert_eq!(done["ok"], true, "{done}");
    let row = db
        .query_one(
            "SELECT status, qty, discount::text, netwr::text FROM public.orders WHERE vbeln = $1",
            &[&doc(7)],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "shipped");
    assert_eq!(row.get::<_, i32>(1), 12);
    assert_eq!(
        row.get::<_, String>(2),
        "12.50",
        "a NUMERIC is cast, not rounded through a float"
    );
    assert_eq!(
        row.get::<_, String>(3),
        "107.00",
        "the read-only column is untouched"
    );
    assert_eq!(touches(&db).await, 1, "the trigger saw exactly one UPDATE");
    let shipped: i64 = db
        .query_one(
            "SELECT count(*) FROM public.orders WHERE status = 'shipped'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(shipped, 1, "no other row changed");
    assert_eq!(
        super::remote_support::metric(&g.p, r#"escurel_write_back_total{outcome="applied"}"#).await,
        Some(1.0)
    );
}

#[tokio::test]
async fn a_row_changed_under_the_draft_conflicts_and_racing_promotes_update_once() {
    let g = Gw::start().await;
    let db = g.pg.client().await;
    // Conflict.
    let e = g.etag(7).await;
    let id = draft_id(&g.draft(7, "status: shipped", &e).await);
    db.execute(
        "UPDATE public.orders SET status = 'on_hold' WHERE vbeln = $1",
        &[&doc(7)],
    )
    .await
    .unwrap();
    let done = g.promote(&id).await;
    assert_eq!(done["ok"], false, "{done}");
    assert!(
        codes(&done).contains(&"write_back_conflict".to_owned()),
        "{done}"
    );
    let status: String = db
        .query_one(
            "SELECT status FROM public.orders WHERE vbeln = $1",
            &[&doc(7)],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, "on_hold", "the other change stands");

    // Racing promotes of ANOTHER draft: the trigger counts one update for it.
    let before: i64 = db
        .query_one("SELECT count(*) FROM public.touch_log", &[])
        .await
        .unwrap()
        .get(0);
    let e8 = g.etag(8).await;
    let id8 = draft_id(&g.draft(8, "status: shipped", &e8).await);
    let (a, b) = tokio::join!(g.promote(&id8), g.promote(&id8));
    assert!(a["ok"] == true || b["ok"] == true, "{a} / {b}");
    let after: i64 = db
        .query_one("SELECT count(*) FROM public.touch_log", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(after - before, 1, "two promotes, one UPDATE");
}

#[tokio::test]
async fn hostile_values_are_data_and_the_table_survives() {
    let g = Gw::start().await;
    let db = g.pg.client().await;
    let e = g.etag(7).await;
    let nasty = "x'); DROP TABLE public.orders; --";
    let id = draft_id(&g.draft(7, &format!("status: \"{nasty}\""), &e).await);
    assert_eq!(g.promote(&id).await["ok"], true);
    let status: String = db
        .query_one(
            "SELECT status FROM public.orders WHERE vbeln = $1",
            &[&doc(7)],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, nasty, "stored verbatim");
    let n: i64 = db
        .query_one("SELECT count(*) FROM public.orders", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n as usize, ROWS, "the table is intact");
    // A value that does not fit the column is the database's refusal, not a crash.
    let e8 = g.etag(8).await;
    let bad = draft_id(&g.draft(8, "quantity: \"not a number\"", &e8).await);
    let done = g.promote(&bad).await;
    assert_eq!(done["ok"], false, "{done}");
    assert!(
        codes(&done).contains(&"write_back_rejected".to_owned()),
        "{done}"
    );
}

#[tokio::test]
async fn a_database_that_is_down_dead_letters_and_comes_back_for_a_later_promote() {
    let g = Gw::start().await;
    let db = g.pg.client().await;
    let e = g.etag(7).await;
    let id = draft_id(&g.draft(7, "status: shipped", &e).await);

    g.pg.stop();
    let failed = g.promote(&id).await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert!(
        codes(&failed).contains(&"write_back_failed".to_owned()),
        "an unreachable database is a dead letter, not a conflict: {failed}"
    );
    let counted =
        super::remote_support::metric(&g.p, r#"escurel_write_back_total{outcome="failed"}"#)
            .await
            .unwrap_or(0.0)
            + super::remote_support::metric(
                &g.p,
                r#"escurel_write_back_total{outcome="dead_letter"}"#,
            )
            .await
            .unwrap_or(0.0);
    assert!(counted >= 1.0, "the outage is counted");

    g.pg.restart().await;
    drop(db);
    let db = g.pg.client().await;
    let status: String = db
        .query_one(
            "SELECT status FROM public.orders WHERE vbeln = $1",
            &[&doc(7)],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, "open", "nothing was applied during the outage");
    let done = g.promote(&id).await;
    assert_eq!(
        done["ok"], true,
        "the same draft lands once the database is back: {done}"
    );
    let status: String = db
        .query_one(
            "SELECT status FROM public.orders WHERE vbeln = $1",
            &[&doc(7)],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, "shipped");
}

#[tokio::test]
async fn a_database_host_in_a_private_range_is_refused_before_any_connection() {
    let g = Gw::start().await;
    // A second credential whose DSN names a metadata/private address: the attach policy refuses it by
    // name, immediately (a connection attempt to 169.254.169.254 would hang).
    let secret_dir = &g._dirs[2];
    let f = secret_dir.path().join("evil-pg");
    std::fs::write(
        &f,
        "host=169.254.169.254 port=5432 user=u password=p dbname=x\n",
    )
    .unwrap();
    let reg = g
        .admin(
            "register_credential",
            json!({ "name": "evil_pg", "connector": "postgres",
                    "secret_ref": format!("file:{}", f.display()) }),
        )
        .await;
    assert_eq!(reg["ok"], true, "registration is lexical: {reg}");
    let skill = skill_page()
        .replace("id: pg-order", "id: evil-order")
        .replace("attach: shop_pg", "attach: evil_pg")
        .replace("# pg-order", "# evil-order");
    let r = g
        .admin(
            "update_page",
            json!({ "page_id": "markdown/skills/evil-order.md", "content": skill }),
        )
        .await;
    let _ = r;
    let started = std::time::Instant::now();
    let refused = raw_call(
        &g.p,
        &g.p.mint_token(TENANT, Role::Admin),
        "list_instances",
        json!({ "skill": "evil-order", "limit": 5 }),
    )
    .await;
    assert!(
        refused.to_string().contains("egress policy"),
        "the policy names itself: {refused}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "refused before any connection attempt ({:?})",
        started.elapsed()
    );
}

/// A second `rows` skill over another relation of the same database.
async fn bind(g: &Gw, id: &str, key: &str, relation: &str, project: &str) {
    let page = format!(
        "---\nkind: skill\nid: {id}\ndescription: {id} rows\nbackend:\n  kind: sql_view\n  instances: rows\n  key: {key}\n  linked: markdown\n  source: {{connector: postgres, attach: shop_pg, relation: \"{relation}\"}}\n  project: {{{project}}}\n---\n# {id}\n"
    );
    let r = g
        .admin(
            "update_page",
            json!({ "page_id": format!("markdown/skills/{id}.md"), "content": page }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
}

async fn page_through(g: &Gw, skill: &str, limit: usize) -> Vec<String> {
    let mut ids = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let r = g
            .admin(
                "list_instances",
                json!({ "skill": skill, "limit": limit, "cursor": cursor }),
            )
            .await;
        for i in r["instances"].as_array().expect("instances") {
            ids.push(i["page_id"].as_str().unwrap_or_default().to_owned());
        }
        match r["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return ids,
        }
    }
}

#[tokio::test]
async fn keys_of_other_types_and_null_keys_page_without_loss_or_repeat() {
    let g = Gw::start().await;
    // A TIMESTAMP key (unique per row) and a table with a NULL key: no row twice, none skipped, and
    // the NULL-keyed row is not an instance (it has no identity).
    bind(
        &g,
        "pg-by-time",
        "created",
        "public.orders",
        "created: created, vbeln: sales_doc",
    )
    .await;
    let ids = page_through(&g, "pg-by-time", 97).await;
    let uniq: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(
        (ids.len(), uniq.len()),
        (ROWS, ROWS),
        "timestamp key, 97 per page"
    );
    bind(
        &g,
        "pg-loose",
        "code",
        "public.loose",
        "code: code, label: label",
    )
    .await;
    let loose = page_through(&g, "pg-loose", 1).await;
    assert_eq!(loose.len(), 2, "the NULL-keyed row is skipped: {loose:?}");
}

#[tokio::test]
async fn a_slow_source_is_interrupted_by_the_statement_timeout_and_the_gateway_stays_usable() {
    let g = Gw::start_with(Some(Duration::from_secs(2))).await;
    g.pg.client()
        .await
        .batch_execute(
            "CREATE VIEW public.slow AS SELECT i::text AS id, pg_sleep(0.3)::text AS z FROM generate_series(1, 200) i",
        )
        .await
        .unwrap();
    bind(&g, "pg-slow", "id", "public.slow", "id: ident").await;
    let started = std::time::Instant::now();
    let refused = raw_call(
        &g.p,
        &g.p.mint_token(TENANT, Role::Admin),
        "list_instances",
        json!({ "skill": "pg-slow", "limit": 200 }),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "interrupted, not 60 s of sleeping ({:?})",
        started.elapsed()
    );
    assert!(
        refused.to_string().contains("did not answer"),
        "the error says what happened: {refused}"
    );
    // The gateway still answers.
    assert!(g.etag(3).await.starts_with("w1:"));
}

#[tokio::test]
async fn another_tenant_cannot_see_these_rows() {
    let g = Gw::start().await;
    let other = raw_call(
        &g.p,
        &g.p.mint_token("globex", Role::Admin),
        "list_instances",
        json!({ "skill": "pg-order", "limit": 5 }),
    )
    .await;
    let seen = other["result"]["structuredContent"]["instances"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(
        seen, 0,
        "a token for another tenant reaches nothing: {other}"
    );
}

#[tokio::test]
async fn a_change_that_committed_but_lost_its_witness_completes_without_a_second_update() {
    let g = Gw::start().await;
    let db = g.pg.client().await;
    let e = g.etag(7).await;
    let id = draft_id(&g.draft(7, "status: shipped", &e).await);
    // The crash window: the UPDATE committed, the gateway died before it wrote the witness. From the
    // gateway's side the draft is still open and the row already holds the change.
    db.execute(
        "UPDATE public.orders SET status = 'shipped' WHERE vbeln = $1",
        &[&doc(7)],
    )
    .await
    .unwrap();
    let before = touches(&db).await;

    let done = g.promote(&id).await;

    assert_eq!(
        done["ok"], true,
        "recognised as applied, not a conflict: {done}"
    );
    assert_eq!(touches(&db).await, before, "and no second UPDATE was sent");
}

/// A TCP listener that completes the handshake (the kernel accepts) and never answers: the
/// behaviour of a black-holed or firewalled database host as libpq sees it.
struct Blackhole {
    _l: TcpListener,
    port: u16,
}

impl Blackhole {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        Self { _l: l, port }
    }
}

// Round-2 review: a blackholed Postgres host stalled the tenant's whole index connection (ATTACH ran
// under `indexer.conn.lock()` with no timeout; DuckDB's interrupt cannot cancel a libpq connect).
// libpq's `connect_timeout` bounds it, and the blocking ATTACH must not occupy the async runtime:
// this runs on ONE runtime worker, and `/healthz` has to answer while the attach is stuck.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_blackholed_database_host_is_bounded_and_does_not_starve_the_runtime() {
    let hole = Blackhole::start();
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let secret_dir = TempDir::new().unwrap();
    let secret: PathBuf = secret_dir.path().join("shop-pg");
    std::fs::write(
        &secret,
        format!(
            "host=127.0.0.1 port={} user=postgres password=x dbname=postgres\n",
            hole.port
        ),
    )
    .unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(
        Indexer::new(store, embedder, conn, TENANT)
            .unwrap()
            .with_sql_connect_timeout(Duration::from_secs(3)),
    );
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            egress: Some(policy(secret_dir.path())),
            signing: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let token = p.mint_token(TENANT, Role::Admin);
    let reg = call_as(
        &p,
        &token,
        "register_credential",
        json!({ "name": "shop_pg", "connector": "postgres",
                "secret_ref": format!("file:{}", secret.display()) }),
    )
    .await;
    assert_eq!(reg["ok"], true, "{reg}");
    let r = call_as(
        &p,
        &token,
        "update_page",
        json!({ "page_id": "markdown/skills/pg-order.md", "content": skill_page() }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");

    let started = std::time::Instant::now();
    let stuck = {
        let (url, token) = (p.mcp_url(), token.clone());
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {token}"))
                .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":"list_instances","arguments":{"skill":"pg-order","limit":5}}}))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let health = tokio::time::timeout(
        Duration::from_secs(2),
        reqwest::get(format!("{}/healthz", p.base_url())),
    )
    .await
    .expect("/healthz must answer while a source attach is stuck")
    .unwrap();
    assert!(health.status().is_success());

    let body = tokio::time::timeout(Duration::from_secs(20), stuck)
        .await
        .expect("the connect is bounded by connect_timeout, not the OS TCP timeout")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        body.contains("backend_unavailable") || body.contains("error"),
        "{body}"
    );
}
