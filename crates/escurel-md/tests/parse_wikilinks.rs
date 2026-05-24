//! Integration tests for `escurel_md::wikilink::parse_wikilinks`.
//!
//! Real markdown fixture on disk, no mocks. Each test exercises one
//! property of the parser; the fixture is a realistic page mixing
//! typed and bare links with inline + fenced code distractors.

use std::fs;
use std::path::PathBuf;

use escurel_md::wikilink::{WikilinkParsed, parse_wikilinks};

fn fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect();
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()))
}

fn parse_first(s: &str) -> WikilinkParsed {
    let links = parse_wikilinks(s);
    assert_eq!(links.len(), 1, "expected exactly 1 wikilink, got {links:?}");
    links.into_iter().next().unwrap()
}

#[test]
fn bare_link() {
    let got = parse_first("see [[error-catalogue]] for codes.\n");
    assert_eq!(got.skill, None);
    assert_eq!(got.id.as_deref(), Some("error-catalogue"));
    assert_eq!(got.anchor, None);
    assert_eq!(got.version, None);
    assert_eq!(got.alias, None);
}

#[test]
fn typed_link() {
    let got = parse_first("see [[customer::acme-corp]] notes.\n");
    assert_eq!(got.skill.as_deref(), Some("customer"));
    assert_eq!(got.id.as_deref(), Some("acme-corp"));
}

#[test]
fn typed_with_anchor() {
    let got = parse_first("ref [[meeting::2026-04-12-acme-qbr#blk-acme-signals]] block.\n");
    assert_eq!(got.skill.as_deref(), Some("meeting"));
    assert_eq!(got.id.as_deref(), Some("2026-04-12-acme-qbr"));
    assert_eq!(got.anchor.as_deref(), Some("blk-acme-signals"));
    assert_eq!(got.version, None);
}

#[test]
fn typed_with_version() {
    let got = parse_first("[[contract::acme-2025-renewal@v3]]\n");
    assert_eq!(got.skill.as_deref(), Some("contract"));
    assert_eq!(got.id.as_deref(), Some("acme-2025-renewal"));
    assert_eq!(got.version.as_deref(), Some("v3"));
}

#[test]
fn typed_with_alias() {
    let got = parse_first("[[customer::acme-corp|Acme Corp]] one-pager.\n");
    assert_eq!(got.skill.as_deref(), Some("customer"));
    assert_eq!(got.id.as_deref(), Some("acme-corp"));
    assert_eq!(got.alias.as_deref(), Some("Acme Corp"));
}

#[test]
fn typed_with_anchor_version_alias() {
    let got = parse_first("[[person::erika-mustermann#blk-on-call@v2|Erika (on-call)]]\n");
    assert_eq!(got.skill.as_deref(), Some("person"));
    assert_eq!(got.id.as_deref(), Some("erika-mustermann"));
    assert_eq!(got.anchor.as_deref(), Some("blk-on-call"));
    assert_eq!(got.version.as_deref(), Some("v2"));
    assert_eq!(got.alias.as_deref(), Some("Erika (on-call)"));
}

#[test]
fn inline_code_is_skipped() {
    let s = "Try `[[not-a-link]]` versus [[real::link]] in prose.\n";
    let links = parse_wikilinks(s);
    assert_eq!(links.len(), 1, "inline-code wikilink must be skipped");
    assert_eq!(links[0].skill.as_deref(), Some("real"));
    assert_eq!(links[0].id.as_deref(), Some("link"));
}

#[test]
fn fenced_code_is_skipped() {
    let s = "outside [[a::x]]\n\n```text\n[[fenced::skipped]] [[also::skipped]]\n```\n\nafter [[b::y]]\n";
    let links = parse_wikilinks(s);
    assert_eq!(
        links.len(),
        2,
        "fenced wikilinks must be skipped; got {links:?}",
    );
    assert_eq!(links[0].skill.as_deref(), Some("a"));
    assert_eq!(links[1].skill.as_deref(), Some("b"));
}

#[test]
fn full_fixture_extracts_seven_links_in_order() {
    let input = fixture("wikilinks_demo.md");
    let links = parse_wikilinks(&input);

    // Expected (in document order): see the fixture body.
    let expected = vec![
        WikilinkParsed {
            skill: Some("customer".into()),
            id: Some("acme-corp".into()),
            anchor: None,
            version: None,
            alias: None,
        },
        WikilinkParsed {
            skill: Some("meeting".into()),
            id: Some("2026-04-12-acme-qbr".into()),
            anchor: Some("blk-acme-signals".into()),
            version: None,
            alias: None,
        },
        WikilinkParsed {
            skill: Some("contract".into()),
            id: Some("acme-2025-renewal".into()),
            anchor: None,
            version: Some("v3".into()),
            alias: None,
        },
        WikilinkParsed {
            skill: None,
            id: Some("error-catalogue".into()),
            anchor: None,
            version: None,
            alias: None,
        },
        WikilinkParsed {
            skill: Some("customer".into()),
            id: Some("acme-corp".into()),
            anchor: None,
            version: None,
            alias: Some("Acme Corp".into()),
        },
        WikilinkParsed {
            skill: Some("person".into()),
            id: Some("erika-mustermann".into()),
            anchor: Some("blk-on-call".into()),
            version: Some("v2".into()),
            alias: Some("Erika (on-call)".into()),
        },
        WikilinkParsed {
            skill: Some("note".into()),
            id: Some("wrap-up-2026-w20".into()),
            anchor: None,
            version: None,
            alias: None,
        },
    ];

    assert_eq!(
        links, expected,
        "extracted wikilinks (in order) do not match expected"
    );
}

#[test]
fn empty_or_unmatched_brackets_yield_nothing() {
    // `[[]]` empty contents; `[[abc]` single closing; `[abc]]` single opening.
    let s = "[[]] and [[abc] and [abc]] yield no links.\n";
    assert!(parse_wikilinks(s).is_empty());
}

#[test]
fn frontmatter_link_values_are_extracted() {
    // The frontmatter parser keeps everything as YAML strings, but
    // the wikilink parser should still pick up `[[…]]` from the
    // page if we pass it the full file content. (The indexer will
    // run the wikilink parser over the body, but here we exercise
    // the property at the parser level: nothing about the parser
    // gates on "must be in the body".)
    let s = "---\nwith: \"[[customer::acme-corp]]\"\n---\n\nbody [[meeting::m1]]\n";
    let links = parse_wikilinks(s);
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].skill.as_deref(), Some("customer"));
    assert_eq!(links[1].skill.as_deref(), Some("meeting"));
}
