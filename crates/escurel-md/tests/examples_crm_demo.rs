//! Verifies every page under `examples/crm-demo/` parses with
//! `escurel_md::parse()` and carries the `type:` matching its
//! subdirectory. The example tenant is the seed corpus for
//! `apps/escurel-explore/`'s fixture mode; if it stops parsing, the
//! UI's offline demo breaks before any user notices.
//!
//! This is a regression gate, not a coverage check — the parser's
//! per-shape unit tests live alongside it in `src/lib.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use escurel_md::{PageType, parse};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/crates/escurel-md`; go up two.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root resolvable from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn collect_pages(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(|entry| {
            let p = entry.ok()?.path();
            (p.extension().is_some_and(|e| e == "md")).then_some(p)
        })
        .collect()
}

#[test]
fn every_skill_page_parses_with_type_skill() {
    let dir = workspace_root().join("examples/crm-demo/skills");
    let pages = collect_pages(&dir);
    assert!(
        !pages.is_empty(),
        "no skill pages found under {}",
        dir.display()
    );

    for path in pages {
        let body = fs::read_to_string(&path).expect("read skill page");
        let page = parse(&body).unwrap_or_else(|e| {
            panic!("parse {}: {e}", path.display());
        });
        assert_eq!(
            page.frontmatter.page_type,
            PageType::Skill,
            "{}: expected type: skill",
            path.display(),
        );
    }
}

#[test]
fn every_instance_page_parses_with_type_instance() {
    let dir = workspace_root().join("examples/crm-demo/instances");
    let pages = collect_pages(&dir);
    assert!(
        !pages.is_empty(),
        "no instance pages found under {}",
        dir.display()
    );

    for path in pages {
        let body = fs::read_to_string(&path).expect("read instance page");
        let page = parse(&body).unwrap_or_else(|e| {
            panic!("parse {}: {e}", path.display());
        });
        assert_eq!(
            page.frontmatter.page_type,
            PageType::Instance,
            "{}: expected type: instance",
            path.display(),
        );
    }
}
