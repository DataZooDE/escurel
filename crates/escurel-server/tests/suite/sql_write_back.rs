//! Write-back to a row of a REAL SQL database (SQLite file read AND written through DuckDB's
//! `sqlite` extension): a human-gated change to a row's writable columns.
//!
//! The model is the REST/MCP one (see `remote_write_back.rs`): a DRAFT carries a `write_back` intent
//! (`patch` + the `base_etag` the proposer saw); nothing reaches the database until a human PROMOTES
//! it; the promote hook re-reads the row and refuses on a changed etag, writes an audit event BEFORE
//! the UPDATE, applies with bound parameters and a per-column optimistic check inside ONE
//! transaction, and records the outcome as a durable event (the witness that makes a re-promote
//! safe). Every claim here is about what the database file holds afterwards.

use serde_json::{Value, json};

use super::sql_rows_db::{Gw, call_as, db_count, db_exec, db_row, doc, row_page};

async fn etag(g: &Gw, n: usize) -> String {
    let r = g.admin("expand", json!({ "page_id": row_page(n) })).await;
    r["backend_projection"]["etag"]
        .as_str()
        .unwrap_or_else(|| panic!("no etag in the projection: {r}"))
        .to_owned()
}

fn intent(n: usize, patch: &str, base_etag: &str, notes: &str) -> String {
    format!(
        "---\nkind: instance\nid: {}\nskill: shop-order\nwrite_back:\n  patch: {{ {patch} }}\n  base_etag: \"{base_etag}\"\n---\n{notes}\n",
        doc(n)
    )
}

async fn draft(g: &Gw, n: usize, content: &str) -> Value {
    g.admin(
        "create_draft",
        json!({ "target_page_id": row_page(n), "content": content }),
    )
    .await
}

fn draft_id(v: &Value) -> String {
    v["draft"]["draft_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no draft id: {v}"))
        .to_owned()
}

async fn promote(g: &Gw, id: &str) -> Value {
    g.admin("promote_draft", json!({ "draft_id": id })).await
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

async fn events(g: &Gw) -> Vec<Value> {
    g.admin(
        "list_events",
        json!({ "label_skill": "escurel:write-back", "include_system": true }),
    )
    .await["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_promoted_write_back_updates_exactly_one_row_and_nothing_else() {
    let g = Gw::start().await;
    let proj = g.admin("expand", json!({ "page_id": row_page(7) })).await;
    assert_eq!(
        proj["backend_projection"]["writable_columns"],
        json!(["status", "quantity"]),
        "the projection names the writable columns: {proj}"
    );
    assert_eq!(
        proj["backend_projection"]["writable_via"], "write_back",
        "{proj}"
    );
    let before_others = db_count(&g.db, "open");
    let e = etag(&g, 7).await;
    let d = draft(
        &g,
        7,
        &intent(
            7,
            "status: shipped, quantity: 12",
            &e,
            "Shipped after the call.",
        ),
    )
    .await;
    let id = draft_id(&d);
    assert_eq!(
        db_row(&g.db, 7)["status"],
        "open",
        "creating a draft must not touch the database"
    );

    let done = promote(&g, &id).await;

    assert_eq!(done["ok"], true, "{done}");
    let row = db_row(&g.db, 7);
    assert_eq!(
        row["status"], "shipped",
        "the database applied the change: {row}"
    );
    assert_eq!(row["qty"], 12);
    assert_eq!(db_count(&g.db, "shipped"), 1, "exactly one row changed");
    assert_eq!(
        db_count(&g.db, "open"),
        before_others - 1,
        "every other row is untouched"
    );
    // The page reads back from the database, the notes are committed WITHOUT the intent.
    let page = g.admin("expand", json!({ "page_id": row_page(7) })).await;
    assert_eq!(page["frontmatter"]["status"], "shipped", "{page}");
    assert!(
        page["body"]
            .as_str()
            .unwrap()
            .contains("Shipped after the call"),
        "{page}"
    );
    assert!(page["frontmatter"].get("write_back").is_none(), "{page}");
    // The audit trail: applying BEFORE the update, applied after, no values in it.
    let ev = events(&g).await;
    let ids: Vec<&str> = ev.iter().filter_map(|e| e["event_id"].as_str()).collect();
    assert!(
        ids.iter().any(|i| i.ends_with(":applying")) && ids.iter().any(|i| i.ends_with(":applied")),
        "{ids:?}"
    );
    assert!(
        !ev.iter().any(|e| e.to_string().contains("shipped")),
        "values must not be audited: {ev:?}"
    );
}

#[tokio::test]
async fn a_row_that_moved_since_the_draft_conflicts_and_the_database_is_not_written() {
    let g = Gw::start().await;
    let e = etag(&g, 7).await;
    let id = draft_id(&draft(&g, 7, &intent(7, "status: shipped", &e, "n")).await);
    // Someone else changes the row after the draft was made.
    db_exec(
        &g.db,
        &format!(
            "UPDATE s.orders SET status = 'on_hold' WHERE vbeln = '{}'",
            doc(7)
        ),
    );

    let done = promote(&g, &id).await;

    assert_eq!(done["ok"], false, "{done}");
    assert!(
        codes(&done).contains(&"write_back_conflict".to_owned()),
        "{done}"
    );
    assert_eq!(
        db_row(&g.db, 7)["status"],
        "on_hold",
        "the other change stands, ours was never applied"
    );
    assert_eq!(db_count(&g.db, "shipped"), 0);
}

#[tokio::test]
async fn only_writable_columns_can_be_proposed_or_promoted() {
    let g = Gw::start().await;
    let e = etag(&g, 7).await;

    // At create_draft: a projected but not writable column.
    let refused = draft(&g, 7, &intent(7, "net_value: 1.0", &e, "n")).await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        codes(&refused).contains(&"backend_read_only_field".to_owned()),
        "{refused}"
    );

    // At promote: the approver's 'correction' swaps the patch for a non-writable column.
    let id = draft_id(&draft(&g, 7, &intent(7, "status: shipped", &e, "n")).await);
    let corrected = intent(7, "net_value: 1.0", &e, "n");
    let done = g
        .admin(
            "promote_draft",
            json!({ "draft_id": id, "content": corrected }),
        )
        .await;
    assert_eq!(done["ok"], false, "{done}");
    assert!(
        codes(&done).contains(&"backend_read_only_field".to_owned()),
        "{done}"
    );
    let row = db_row(&g.db, 7);
    assert_eq!(
        row["netwr"], 107.0,
        "the read-only column was never written: {row}"
    );

    // A direct update_page may not carry an intent at all: the human gate is the draft.
    let direct = g
        .admin(
            "update_page",
            json!({ "page_id": row_page(7), "content": intent(7, "status: shipped", &e, "n") }),
        )
        .await;
    assert!(
        codes(&direct)
            .iter()
            .any(|c| c == "write_back_requires_draft" || c == "backend_read_only_field"),
        "{direct}"
    );
    assert_eq!(db_row(&g.db, 7)["status"], "open");
}

#[tokio::test]
async fn hostile_values_and_column_names_are_data_never_sql() {
    let g = Gw::start().await;
    let e = etag(&g, 7).await;
    let total: i64 = db_count(&g.db, "open") + db_count(&g.db, "orphan");

    // A value full of SQL is stored as the literal text it is.
    let nasty = "x'); DROP TABLE orders; --";
    let id = draft_id(&draft(&g, 7, &intent(7, &format!("status: \"{nasty}\""), &e, "n")).await);
    let done = promote(&g, &id).await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(db_row(&g.db, 7)["status"], nasty, "stored verbatim");
    assert_eq!(
        db_count(&g.db, "open") + db_count(&g.db, "orphan"),
        total - 1,
        "the table is intact"
    );

    // A 'column name' that is really SQL never gets near the statement.
    let e2 = etag(&g, 8).await;
    let bad = draft(&g, 8, &intent(8, "\"status = 'x', qty\": 1", &e2, "n")).await;
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(db_row(&g.db, 8)["status"], "open");
}

#[tokio::test]
async fn two_promotes_racing_on_one_draft_apply_once() {
    let g = Gw::start().await;
    let e = etag(&g, 7).await;
    let id = draft_id(&draft(&g, 7, &intent(7, "status: shipped", &e, "n")).await);

    let (a, b) = tokio::join!(promote(&g, &id), promote(&g, &id));

    assert!(
        a["ok"] == true || b["ok"] == true,
        "one of them completes: {a} / {b}"
    );
    assert_eq!(db_row(&g.db, 7)["status"], "shipped");
    let applied = events(&g)
        .await
        .iter()
        .filter(|e| {
            e["event_id"]
                .as_str()
                .is_some_and(|i| i.ends_with(":applied"))
        })
        .count();
    assert_eq!(applied, 1, "one applied witness however many promotes race");
}

#[tokio::test]
async fn a_locked_database_dead_letters_and_a_later_promote_succeeds() {
    let g = Gw::start().await;
    let e = etag(&g, 7).await;
    let id = draft_id(&draft(&g, 7, &intent(7, "status: shipped", &e, "n")).await);

    // Another writer holds the database's write lock for as long as this connection is open.
    let locker = duckdb::Connection::open_in_memory().unwrap();
    locker
        .execute_batch("INSTALL sqlite; LOAD sqlite;")
        .unwrap();
    locker
        .execute_batch(&format!(
            "ATTACH '{}' AS s (TYPE sqlite); BEGIN; UPDATE s.orders SET qty = qty WHERE vbeln = '{}';",
            g.db.display(),
            doc(100)
        ))
        .unwrap();

    let failed = promote(&g, &id).await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert!(
        codes(&failed).iter().any(|c| c == "write_back_failed"),
        "a database that cannot be written is a dead letter, not a conflict: {failed}"
    );
    assert_eq!(db_row(&g.db, 7)["status"], "open", "nothing was applied");
    assert!(
        events(&g).await.iter().any(|e| e["event_id"]
            .as_str()
            .is_some_and(|i| i.ends_with(":failed"))),
        "the failure is recorded"
    );

    // The lock goes away; the human promotes again and it lands.
    locker.execute_batch("ROLLBACK;").unwrap();
    drop(locker);
    let done = promote(&g, &id).await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(db_row(&g.db, 7)["status"], "shipped");
}

#[tokio::test]
async fn an_agent_run_token_proposes_but_never_approves_a_database_change() {
    let g = Gw::start().await;
    let e = etag(&g, 7).await;
    let minted = g
        .admin(
            "mint_agent_token",
            json!({ "skill": "shop-order", "target_page_id": row_page(7) }),
        )
        .await["token"]
        .as_str()
        .expect("a minted run token")
        .to_owned();
    let created = call_as(
        &g.p,
        &minted,
        "create_draft",
        json!({ "target_page_id": row_page(7), "content": intent(7, "status: shipped", &e, "n") }),
    )
    .await;
    let id = draft_id(&created);

    let own = call_as(&g.p, &minted, "promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(
        own["ok"], false,
        "the agent must not approve its own change: {own}"
    );
    assert!(
        codes(&own).contains(&"promote_requires_human".to_owned()),
        "{own}"
    );
    assert_eq!(
        db_row(&g.db, 7)["status"],
        "open",
        "nothing reaches the database on an agent's say-so"
    );

    assert_eq!(promote(&g, &id).await["ok"], true, "a person can");
    assert_eq!(db_row(&g.db, 7)["status"], "shipped");
    assert_eq!(
        super::remote_support::metric(&g.p, r#"escurel_write_back_total{outcome="applied"}"#).await,
        Some(1.0),
        "the outcome is counted"
    );
}

#[tokio::test]
async fn a_file_connector_declaring_writable_columns_still_cannot_be_written_back() {
    let g = Gw::start().await;
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("a.json"),
        r#"[{"vbeln":"0000000001","status":"open"}]"#,
    )
    .unwrap();
    let skill = format!(
        "---\nkind: skill\nid: file-order\ndescription: read-only JSON files\nbackend:\n  kind: sql_view\n  instances: rows\n  key: vbeln\n  linked: markdown\n  writable_columns: [status]\n  source: {{connector: json_dir, relation: \"{}\"}}\n  project: {{vbeln: sales_doc, status: status}}\n---\n# file-order\n",
        dir.path().display()
    );
    let r = g
        .admin(
            "update_page",
            json!({ "page_id": "markdown/skills/file-order.md", "content": skill }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let page = "markdown/instances/file-order/0000000001.md";
    let content = "---\nkind: instance\nid: \"0000000001\"\nskill: file-order\nwrite_back:\n  patch: { status: shipped }\n---\nn\n";
    let refused = g
        .admin(
            "create_draft",
            json!({ "target_page_id": page, "content": content }),
        )
        .await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        codes(&refused).contains(&"backend_read_only".to_owned()),
        "a directory of files has nothing to write back to: {refused}"
    );
    let proj = g.admin("expand", json!({ "page_id": page })).await;
    assert!(
        proj["backend_projection"].get("writable_via").is_none(),
        "no writable promise for a read-only source: {proj}"
    );
}
