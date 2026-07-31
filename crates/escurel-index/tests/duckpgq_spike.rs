//! DuckPGQ go/no-go spike (ADR-0010, PR-5).
//!
//! `#[ignore]` by default — it reaches the DuckDB **community** extension
//! registry over the network and loads an UNSIGNED extension, neither of
//! which belongs in the normal `cargo test` gate. Run it explicitly to
//! decide whether the DuckPGQ `MATCH` backend (PR-6) is viable on the
//! pinned `libduckdb`:
//!
//! ```sh
//! cargo test -p escurel-index --test duckpgq_spike -- --ignored --nocapture
//! ```
//!
//! GO  = `INSTALL duckpgq FROM community; LOAD duckpgq;` succeeds AND a
//!       `CREATE PROPERTY GRAPH` over `resolved_links` + a trivial `MATCH`
//!       returns the seeded edge.
//! NO-GO = any of those fails (most likely: no prebuilt duckpgq binary for
//!       this exact libduckdb version). The feature stays complete on the
//!       recursive-CTE backend; revisit on the next DuckDB bump.
//!
//! The outcome is recorded under `docs/notes/discovered/`.

use duckdb::{Config, Connection};
use escurel_index::Migrator;
use tempfile::TempDir;

#[test]
#[ignore = "network + unsigned community extension; run explicitly for the DuckPGQ go/no-go"]
fn duckpgq_loads_and_matches_over_resolved_links() {
    let dir = TempDir::new().expect("tempdir");
    // allow_unsigned_extensions is a STARTUP setting — it cannot be `SET`
    // once the database is running (extensions loaded), so it goes on the
    // open-time Config.
    let config = Config::default()
        .allow_unsigned_extensions()
        .expect("config");
    let conn = Connection::open_with_flags(dir.path().join("spike.duckdb"), config).expect("open");

    // The real per-tenant schema: pages, links, … and the resolved_links
    // VIEW (STAGE_12). We test DuckPGQ against the ACTUAL view shape.
    Migrator::load_extensions(&conn).expect("load vss/fts");
    Migrator::enable_hnsw_persistence(&conn).expect("hnsw flag");
    Migrator::up(&conn).expect("migrate");

    // Seed one resolvable provenance edge: result r --produced_by--> analysis a.
    conn.execute_batch(
        "INSERT INTO pages (page_id, slug, skill, page_type, frontmatter, body_hash, created_at, updated_at) VALUES \
           ('markdown/instances/analysis/a.md', 'a', 'analysis', 'instance', '{}', 'h', now(), now()), \
           ('markdown/instances/result/r.md',   'r', 'result',   'instance', '{}', 'h', now(), now()); \
         INSERT INTO links (src_page, src_anchor, src_field, dst_page, dst_anchor, link_skill) VALUES \
           ('markdown/instances/result/r.md', '', 'frontmatter.produced_by', 'a', '', 'analysis');",
    )
    .expect("seed");

    // Sanity: the view already resolves the edge (this works with or without
    // DuckPGQ — it's the recursive-CTE backend's substrate).
    let via_view: i64 = conn
        .query_row("SELECT count(*) FROM resolved_links", [], |r| r.get(0))
        .expect("view query");
    assert_eq!(via_view, 1, "resolved_links must expose the seeded edge");

    // --- the spike proper (allow_unsigned_extensions set at open) ---------
    // A probe, not a gate: if duckpgq can't load on this libduckdb (the
    // current reality — no prebuilt binary for 1.5.3), record NO-GO and
    // return green. Only when it DOES load do we assert the GO path, so the
    // test starts verifying the MATCH backend the day duckpgq ships for our
    // DuckDB — without rotting red in the meantime.
    if let Err(e) = conn.execute_batch("INSTALL duckpgq FROM community; LOAD duckpgq;") {
        println!(
            "NO-GO: `INSTALL duckpgq FROM community; LOAD duckpgq` failed on this libduckdb: {e}\n\
             The provenance feature stays complete on the recursive-CTE backend; \
             revisit the DuckPGQ MATCH backend on the next DuckDB bump. \
             See docs/notes/discovered/2026-07-31-duckpgq-unavailable-on-1.5.3.md."
        );
        return;
    }

    conn.execute_batch(
        "DROP PROPERTY GRAPH IF EXISTS provenance; \
         CREATE PROPERTY GRAPH provenance \
           VERTEX TABLES (pages KEY (page_id)) \
           EDGE TABLES ( \
             resolved_links \
               SOURCE KEY (src_page_id) REFERENCES pages (page_id) \
               DESTINATION KEY (dst_page_id) REFERENCES pages (page_id) \
           );",
    )
    .expect("NO-GO: CREATE PROPERTY GRAPH over resolved_links failed");

    let matched: i64 = conn
        .query_row(
            "SELECT count(*) FROM GRAPH_TABLE ( \
                 provenance MATCH (a)-[e]->(b) COLUMNS (a.page_id AS src) \
             )",
            [],
            |r| r.get(0),
        )
        .expect("NO-GO: MATCH over the property graph failed");

    assert!(matched >= 1, "GO requires MATCH to see the seeded edge");
    println!(
        "GO: duckpgq installed, loaded, CREATE PROPERTY GRAPH + MATCH all worked ({matched} edge[s]) \
         — the DuckPGQ backend arm is now viable on this libduckdb."
    );
}
