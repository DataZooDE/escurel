# kreuzberg crate requires rust-version 1.91 (workspace pins 1.88)

**Date:** 2026-06-21 · **Area:** document backend (PR-3b)

## Symptom

Adopting the in-process document extractor (change request D8 / HLD §8,
"depend on the `kreuzberg` crate directly") fails at dependency-resolution
time:

```
$ cargo add kreuzberg -p escurel-index --dry-run
error: no version of crate `kreuzberg` can maintain escurel-index's
       rust-version of 1.88
help: pass `--ignore-rust-version` to select kreuzberg@4.9.9 which
      requires rustc 1.91
```

kreuzberg 4.9.9 (the version spike S5 validated) declares
`rust-version = 1.91`. The escurel workspace pins `rust-version = 1.88`
(root `Cargo.toml [workspace.package]`). Cargo's MSRV-aware resolver refuses
the dependency. The *installed* toolchain is newer (1.96), so it would
compile — this is a declared-contract conflict, not a toolchain limit.

## Fix / how to recognise it next time

Wiring the `KreuzbergExtractor` (PDF/DOCX, `bundled-pdfium`) requires a
**workspace MSRV bump to 1.91** in the root `Cargo.toml`. That is a
project-wide policy change (CI images, the substrate golden image, any
external contributor's toolchain), so it is an explicit decision, not an
incidental dependency add. Recognise it by the resolver error above on any
crate that has moved its MSRV ahead of ours.

## What PR-3b did instead

Landed the `Extractor` trait + `ExtractionResult` contract (the kreuzberg
shape) + a real born-digital `PlainTextExtractor` (`text/*`) + `NullExtractor`
for tests. The whole document-ingestion pipeline (PR-3c/3d/3e) builds and is
E2E-tested on the trait with the text extractor — no mocks. The kreuzberg
PDF/DOCX path slots in behind the trait (REQ-NF-08 keeps it swappable) once
the MSRV decision lands. The residual is the PDF/DOCX binary-format coverage
+ the ELv2 acceptance recorded at adoption.
