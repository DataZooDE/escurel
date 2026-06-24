//! E2E for loader metadata enrichment: a per-file sidecar sets extra instance
//! frontmatter (nummer/titel/wp/doctype/…) at materialize time, so a corpus is
//! facetable + carries real titles. Real DuckDB + FsStore + PlainText extractor
//! + offline HashEmbedder, no mocks.

use std::sync::Arc;

use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::backend::{Extractor, PlainTextExtractor};
use escurel_index::schema::Migrator;
use escurel_loader::LoaderBuilder;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn sidecar_metadata_lands_in_instance_frontmatter() {
    let src = TempDir::new().unwrap();
    std::fs::write(
        src.path().join("16_100_D.txt"),
        "Antrag der Fraktion. Inhalt.",
    )
    .unwrap();
    std::fs::write(src.path().join("plain.txt"), "kein Sidecar-Eintrag").unwrap();

    // Sidecar keyed by file name → extra frontmatter.
    let mut sidecar = serde_json::Map::new();
    sidecar.insert(
        "16_100_D.txt".to_string(),
        json!({ "nummer": "16/100", "titel": "Zukunftsfähigkeit sichern", "wp": "16", "doctype": "Antrag" }),
    );

    let out = TempDir::new().unwrap();
    let extractor: Arc<dyn Extractor> = Arc::new(PlainTextExtractor);
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    LoaderBuilder::new(out.path(), "drucksache", extractor, embedder)
        .with_metadata(sidecar)
        .build(src.path())
        .await
        .expect("loader build");

    let conn = duckdb::Connection::open(out.path().join("escurel.duckdb")).unwrap();
    Migrator::load_extensions(&conn).unwrap();

    // The enriched doc carries wp/doctype/titel/nummer frontmatter.
    let (wp, doctype, titel): (String, String, String) = conn
        .query_row(
            "SELECT json_extract_string(frontmatter,'$.wp'), \
                    json_extract_string(frontmatter,'$.doctype'), \
                    json_extract_string(frontmatter,'$.titel') \
             FROM pages WHERE page_type='instance' \
               AND json_extract_string(frontmatter,'$.nummer') = '16/100'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("enriched instance present");
    assert_eq!(wp, "16");
    assert_eq!(doctype, "Antrag");
    assert_eq!(titel, "Zukunftsfähigkeit sichern");

    // The doc without a sidecar entry still materialises (no extra fields).
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM pages WHERE page_type='instance'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "both docs materialised");
}
