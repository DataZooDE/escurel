//! A guard: nothing in the workspace may read a `tools/call` result's `structuredContent` by itself.
//!
//! Reading the field blindly turns a REFUSED call (`isError: true`, payload `{ok: false, issues}`)
//! into data, and a response type whose fields all default then decodes it as an empty SUCCESS: a
//! silent partial read after an access denial. Every consumer opens a result through
//! `escurel_types::call_result` (`unwrap_call_result` / `refusal_of` / `payload_of`).
//!
//! This test scans the non-test sources of every crate for the field name. A file that has a real
//! reason to mention it is listed below WITH the reason; anything else fails, so a new consumer has to
//! use the shared reader (or justify itself here, in review).

use std::fs;
use std::path::{Path, PathBuf};

/// (path relative to `crates/`, why it may mention the field)
const ALLOWED: &[(&str, &str)] = &[
    ("escurel-types/src/call_result.rs", "THE shared reader"),
    (
        "escurel-server/src/mcp.rs",
        "the PRODUCER of the result: builds `structuredContent` and `isError`",
    ),
    (
        "escurel-server/src/remote_backend.rs",
        "reads an UPSTREAM MCP server's result and already refuses on its `isError` first",
    ),
    (
        "escurel-test-support/src/mcp_client.rs",
        "test helper: `call` returns the payload on purpose (tests assert on refusals); `call_ok` goes through the shared reader",
    ),
    (
        "escurel-runner-harness/src/gemini.rs",
        "reads the payload for logging only; `landed` is decided by `call_result::refusal_of`",
    ),
    (
        "escurel-client/src/transport.rs",
        "documentation of the envelope; the payload is read by `call_result`",
    ),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            let name = p.file_name().unwrap().to_string_lossy();
            if name == "target" || name == "tests" || name == "benches" {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn only_the_shared_reader_opens_structured_content() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut offenders = Vec::new();
    for krate in fs::read_dir(crates).unwrap().flatten() {
        let src = krate.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for f in files {
            let text = fs::read_to_string(&f).unwrap();
            if !(text.contains("structuredContent") || text.contains("structured_content")) {
                continue;
            }
            let rel = f
                .strip_prefix(crates)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if !ALLOWED.iter().any(|(p, _)| *p == rel) {
                offenders.push(rel);
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these files read `structuredContent` themselves; open a tools/call result through \
         `escurel_types::call_result::unwrap_call_result` (a refusal must be an error, not an empty \
         success), or add the file to ALLOWED with the reason: {offenders:#?}"
    );
}
