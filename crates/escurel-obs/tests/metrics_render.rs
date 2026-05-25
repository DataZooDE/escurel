//! Real-registry exercises for [`escurel_obs::Metrics`].
//!
//! No mocks: each test constructs a real `Metrics` (a real Prometheus
//! `Registry`), mutates it through the public API, and asserts against
//! the real text-format render. `Metrics::new()` builds an isolated
//! registry per instance, so these tests share no global state.

use escurel_obs::Metrics;

#[test]
fn render_prometheus_emits_help_and_type_lines() {
    let m = Metrics::new();
    // The Prometheus text encoder only emits a label-vec family once it
    // has at least one sample, so touch each family before asserting.
    m.set_up(true);
    m.inc_request("/mcp", 200);
    m.observe_latency("/mcp", 0.01);
    let out = m.render_prometheus();

    // Every metric family must carry HELP + TYPE lines.
    assert!(
        out.contains("# HELP escurel_up"),
        "missing HELP escurel_up:\n{out}"
    );
    assert!(
        out.contains("# TYPE escurel_up gauge"),
        "missing TYPE escurel_up:\n{out}"
    );
    assert!(
        out.contains("# HELP escurel_requests_total"),
        "missing HELP escurel_requests_total:\n{out}"
    );
    assert!(
        out.contains("# TYPE escurel_requests_total counter"),
        "missing TYPE escurel_requests_total:\n{out}"
    );
    assert!(
        out.contains("# TYPE escurel_request_latency_seconds histogram"),
        "missing TYPE escurel_request_latency_seconds:\n{out}"
    );
}

#[test]
fn inc_request_increments_counter_visible_in_render() {
    let m = Metrics::new();
    m.inc_request("/mcp", 200);
    m.inc_request("/mcp", 200);
    m.inc_request("/healthz", 200);

    let out = m.render_prometheus();
    // Two hits on /mcp with status 200.
    assert!(
        out.contains(r#"escurel_requests_total{route="/mcp",status="200"} 2"#),
        "expected /mcp count 2:\n{out}"
    );
    assert!(
        out.contains(r#"escurel_requests_total{route="/healthz",status="200"} 1"#),
        "expected /healthz count 1:\n{out}"
    );
}

#[test]
fn observe_latency_emits_histogram_buckets() {
    let m = Metrics::new();
    m.observe_latency("/mcp", 0.011);
    m.observe_latency("/mcp", 0.250);

    let out = m.render_prometheus();
    assert!(
        out.contains(r#"escurel_request_latency_seconds_bucket{route="/mcp","#),
        "missing histogram buckets:\n{out}"
    );
    assert!(
        out.contains(r#"escurel_request_latency_seconds_count{route="/mcp"} 2"#),
        "expected histogram count 2:\n{out}"
    );
    assert!(
        out.contains(r#"escurel_request_latency_seconds_sum{route="/mcp"}"#),
        "missing histogram sum:\n{out}"
    );
}

#[test]
fn set_up_toggles_escurel_up_gauge() {
    let m = Metrics::new();

    m.set_up(true);
    let out = m.render_prometheus();
    assert!(
        out.contains("escurel_up 1"),
        "expected escurel_up 1:\n{out}"
    );

    m.set_up(false);
    let out = m.render_prometheus();
    assert!(
        out.contains("escurel_up 0"),
        "expected escurel_up 0:\n{out}"
    );
}

#[test]
fn render_is_valid_prometheus_text_format() {
    let m = Metrics::new();
    m.set_up(true);
    m.inc_request("/mcp", 200);
    m.observe_latency("/mcp", 0.02);

    let out = m.render_prometheus();

    // Each HELP must be followed by a TYPE for the same family, and
    // every non-comment, non-blank line must be a well-formed sample:
    // `name{labels}? value` (value is the last whitespace-separated
    // token and must parse as f64).
    for line in out.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let value = line
            .rsplit(char::is_whitespace)
            .next()
            .expect("sample line has a value token");
        assert!(
            value.parse::<f64>().is_ok(),
            "sample value not numeric in line: {line:?}"
        );
    }

    // HELP lines pair with TYPE lines.
    let help_count = out.lines().filter(|l| l.starts_with("# HELP")).count();
    let type_count = out.lines().filter(|l| l.starts_with("# TYPE")).count();
    assert_eq!(
        help_count, type_count,
        "HELP/TYPE line counts differ:\n{out}"
    );
    assert!(
        help_count >= 3,
        "expected at least 3 metric families:\n{out}"
    );
}
