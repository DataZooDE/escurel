//! Integration tests for `escurel_md::parse`.
//!
//! Real markdown fixtures on disk, no mocks. Each fixture exercises
//! a representative shape from the spec.

use std::fs;
use std::path::PathBuf;

use escurel_md::{PageType, ParseError, parse};

fn fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect();
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()))
}

#[test]
fn parses_skill_page() {
    let input = fixture("skill_customer.md");
    let page = parse(&input).expect("skill fixture must parse");

    assert_eq!(page.frontmatter.page_type, PageType::Skill);

    let fields = &page.frontmatter.fields;
    assert_eq!(fields["id"].as_str(), Some("customer"));
    assert!(
        fields["description"]
            .as_str()
            .unwrap_or_default()
            .starts_with("A buying entity"),
        "description should start with 'A buying entity', got: {:?}",
        fields["description"]
    );

    let required = fields["required_frontmatter"]
        .as_sequence()
        .expect("required_frontmatter is a YAML sequence");
    let required_strs: Vec<&str> = required
        .iter()
        .filter_map(serde_yaml_ng::Value::as_str)
        .collect();
    assert_eq!(required_strs, vec!["tier", "opened", "status"]);

    assert!(page.body.contains("# customer"));
    assert!(
        page.body.starts_with('\n'),
        "body keeps its leading newline"
    );
}

#[test]
fn parses_instance_page() {
    let input = fixture("instance_acme.md");
    let page = parse(&input).expect("instance fixture must parse");

    assert_eq!(page.frontmatter.page_type, PageType::Instance);

    let fields = &page.frontmatter.fields;
    assert_eq!(fields["skill"].as_str(), Some("customer"));
    assert_eq!(fields["id"].as_str(), Some("acme-corp"));
    assert_eq!(fields["tier"].as_str(), Some("enterprise"));
    assert_eq!(fields["status"].as_str(), Some("active"));
    assert_eq!(fields["mrr_band"].as_str(), Some("100k-250k"));

    assert!(page.body.contains("# Acme Corp"));
}

#[test]
fn parses_event_typed_instance() {
    let input = fixture("event_meeting.md");
    let page = parse(&input).expect("event fixture must parse");

    assert_eq!(page.frontmatter.page_type, PageType::Instance);

    let fields = &page.frontmatter.fields;
    assert_eq!(fields["skill"].as_str(), Some("meeting"));
    // `at` arrives as an RFC 3339 string. Strict timestamp parsing
    // (and type-coercing it to a TIMESTAMP at the indexer layer) is
    // a downstream concern; this PR keeps it as a YAML string.
    assert_eq!(
        fields["at"].as_str(),
        Some("2026-04-12T10:00:00+02:00"),
        "at must come through as the RFC 3339 string verbatim"
    );

    let participants = fields["participants"]
        .as_sequence()
        .expect("participants is a YAML sequence");
    assert_eq!(participants.len(), 2);
}

#[test]
fn malformed_yaml_returns_yaml_error() {
    let input = fixture("malformed_frontmatter.md");
    let err = parse(&input).expect_err("malformed fixture must fail to parse");
    assert!(
        matches!(err, ParseError::Yaml(_)),
        "expected ParseError::Yaml, got: {err:?}",
    );
}

#[test]
fn input_without_leading_delimiter_errors() {
    let input = "no frontmatter here\nhello\n";
    let err = parse(input).expect_err("input without frontmatter must fail");
    assert!(
        matches!(err, ParseError::MissingFrontmatter),
        "expected MissingFrontmatter, got: {err:?}",
    );
}

#[test]
fn unterminated_frontmatter_errors() {
    let input = "---\ntype: skill\nid: customer\n\nstill no closing delimiter\n";
    let err = parse(input).expect_err("unterminated frontmatter must fail");
    assert!(
        matches!(err, ParseError::UnterminatedFrontmatter),
        "expected UnterminatedFrontmatter, got: {err:?}",
    );
}

#[test]
fn frontmatter_must_be_a_mapping() {
    // YAML sequence at the top level — valid YAML, but not a mapping.
    let input = "---\n- one\n- two\n---\n\nbody\n";
    let err = parse(input).expect_err("sequence frontmatter must fail");
    assert!(
        matches!(err, ParseError::NotAMapping),
        "expected NotAMapping, got: {err:?}",
    );
}

#[test]
fn missing_type_field_errors() {
    let input = "---\nid: customer\ndescription: oops\n---\n\nbody\n";
    let err = parse(input).expect_err("missing type must fail");
    assert!(
        matches!(err, ParseError::InvalidType),
        "expected InvalidType, got: {err:?}",
    );
}

#[test]
fn unknown_type_value_errors() {
    let input = "---\ntype: gadget\nid: x\n---\n\nbody\n";
    let err = parse(input).expect_err("unknown type must fail");
    assert!(
        matches!(err, ParseError::InvalidType),
        "expected InvalidType, got: {err:?}",
    );
}

#[test]
fn set_frontmatter_bool_stamps_flag_and_preserves_body() {
    // #300: stamp `archived: true` onto an existing page; the flag round-trips
    // through parse and the body is preserved verbatim.
    let input = "---\ntype: instance\nskill: customer\nid: acme\n---\n# Acme\n\nBody text.\n";
    let out =
        escurel_md::set_frontmatter_bool(input, "archived", true).expect("stamp archived flag");

    let page = parse(&out).expect("re-parse stamped page");
    assert_eq!(page.frontmatter.page_type, PageType::Instance);
    assert_eq!(
        page.frontmatter.fields["archived"].as_bool(),
        Some(true),
        "archived flag must be present and true"
    );
    // Original keys survive the round-trip.
    assert_eq!(page.frontmatter.fields["id"].as_str(), Some("acme"));
    assert_eq!(page.frontmatter.fields["skill"].as_str(), Some("customer"));
    // Body is unchanged.
    assert_eq!(page.body, "# Acme\n\nBody text.\n");
}

#[test]
fn set_frontmatter_bool_rejects_malformed_input() {
    let err = escurel_md::set_frontmatter_bool("no frontmatter here", "archived", true)
        .expect_err("malformed input must error");
    assert!(matches!(err, ParseError::MissingFrontmatter));
}
