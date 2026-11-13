//! The shared SKILL.md fixtures (#825, crew F11): every
//! `tests/fixtures/skill-md/<case>/page.md` exports to exactly its `SKILL.md`.
//! The VS Code exporter (`editors/vscode/test/unit/skillMd.test.ts`) reads
//! the same directories, so the two renderings cannot drift apart unnoticed.

use std::path::{Path, PathBuf};

use escurel_md::skill_md::{export, import};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skill-md")
}

fn read(case: &str, file: &str) -> String {
    std::fs::read_to_string(root().join(case).join(file))
        .unwrap_or_else(|e| panic!("{case}/{file}: {e}"))
}

/// Every case, and whether importing its SKILL.md gives the page back byte
/// for byte. `no-description` gains the description it exported with; `crlf`
/// comes back with an LF frontmatter (its body keeps its CRLF).
const CASES: &[(&str, bool)] = &[
    ("triage", true),
    ("crlf", false),
    ("no-description", false),
    ("passthrough-only", true),
    ("no-links", true),
];

#[test]
fn every_case_on_disk_is_listed() {
    let mut on_disk: Vec<String> = std::fs::read_dir(root())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = CASES.iter().map(|(c, _)| (*c).to_owned()).collect();
    listed.sort();
    assert_eq!(on_disk, listed);
}

#[test]
fn each_page_exports_to_its_skill_md() {
    for (case, lossless) in CASES {
        let page = read(case, "page.md");
        let want = read(case, "SKILL.md");
        let out = export(&page).unwrap_or_else(|e| panic!("{case}: {e}"));
        assert_eq!(out.skill_md, want, "{case}: actual:\n{}", out.skill_md);
        let back = import(&want).unwrap_or_else(|e| panic!("{case}: {e}"));
        if *lossless {
            assert_eq!(back.page, page, "{case}: import is the inverse");
        }
    }
}

#[test]
fn a_crlf_body_keeps_its_line_endings_both_ways() {
    let page = read("crlf", "page.md");
    assert!(
        page.contains("\r\n"),
        "the fixture must be checked out with CRLF"
    );
    let md = export(&page).unwrap().skill_md;
    assert!(md.contains("# windows\r\n\r\nCheck [[customer::acme]] first.\r\n"));
    let back = import(&md).unwrap().page;
    assert!(back.ends_with("# windows\r\n\r\nCheck [[customer::acme]] first.\r\nThen answer.\r\n"));
}

#[test]
fn a_page_without_a_description_exports_its_summary() {
    let back = import(&read("no-description", "SKILL.md")).unwrap().page;
    assert!(
        back.contains("description: Only a summary, no description.\n"),
        "{back}"
    );
    assert!(
        back.contains("summary: Only a summary, no description.\n"),
        "{back}"
    );
}
