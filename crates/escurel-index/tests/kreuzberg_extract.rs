//! Real PDF/DOCX extraction via the in-process kreuzberg crate (PR-3f).
//! Runs only under `--features kreuzberg` (the heavy, ELv2-licensed native
//! dependency is off by default). No mocks — real born-digital fixtures.
#![cfg(feature = "kreuzberg")]

use escurel_index::backend::{
    ChunkConfig, ExtractConfig, ExtractError, Extractor, KreuzbergExtractor, OcrPolicy,
};

const PDF_MIME: &str = "application/pdf";
const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

fn cfg() -> ExtractConfig {
    ExtractConfig {
        ocr: OcrPolicy::Off,
        chunk: ChunkConfig {
            max_chars: 300,
            overlap: 60,
        },
    }
}

#[tokio::test]
async fn extracts_born_digital_pdf_with_chunks_and_metadata() {
    let ex = KreuzbergExtractor;
    assert!(ex.accepts(PDF_MIME));
    let bytes = include_bytes!("fixtures/report.pdf");
    let r = ex
        .extract(bytes, PDF_MIME, &cfg())
        .await
        .expect("extract pdf");

    assert!(
        r.content.contains("Quarterly"),
        "content: {:?}",
        &r.content[..r.content.len().min(80)]
    );
    assert!(
        r.metadata.page_count.unwrap_or(0) >= 1,
        "page_count {:?}",
        r.metadata.page_count
    );
    assert!(!r.chunks.is_empty(), "expected chunks");
    // Chunk provenance: sequential ordinals + valid byte spans.
    for (i, c) in r.chunks.iter().enumerate() {
        assert_eq!(c.ordinal as usize, i, "ordinals must be 0..n sequential");
        assert!(c.byte_end >= c.byte_start, "valid byte span");
    }
}

#[tokio::test]
async fn extracts_docx_content() {
    let ex = KreuzbergExtractor;
    assert!(ex.accepts(DOCX_MIME));
    let bytes = include_bytes!("fixtures/memo.docx");
    let r = ex
        .extract(bytes, DOCX_MIME, &cfg())
        .await
        .expect("extract docx");
    assert!(
        r.content.len() > 20,
        "docx content too short: {} chars",
        r.content.len()
    );
}

#[tokio::test]
async fn corrupt_pdf_is_graceful_not_a_panic() {
    let bytes = include_bytes!("fixtures/corrupt.pdf");
    match KreuzbergExtractor.extract(bytes, PDF_MIME, &cfg()).await {
        // Typed catchable error → the extraction_failed / blob-retention path.
        Err(ExtractError::ExtractionFailed(_)) => {}
        // Some parsers tolerate junk; acceptable only if clearly empty.
        Ok(r) => assert!(
            r.content.trim().is_empty(),
            "corrupt input tolerated only when it yields empty content"
        ),
        Err(other) => panic!("unexpected error variant: {other:?}"),
    }
}

#[tokio::test]
async fn force_ocr_without_runtime_reports_ocr_unavailable() {
    let bytes = include_bytes!("fixtures/report.pdf");
    let force = ExtractConfig {
        ocr: OcrPolicy::Force,
        ..cfg()
    };
    let err = KreuzbergExtractor
        .extract(bytes, PDF_MIME, &force)
        .await
        .expect_err("Force OCR without the ocr feature must fail closed");
    assert!(matches!(err, ExtractError::OcrUnavailable), "got {err:?}");
}
