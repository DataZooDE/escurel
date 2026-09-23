//! Joining a request span to a RUN's trace (knowledge-workbench backend
//! P3-3). A run-bound bearer carries the lineage's `trace_id` claim; every
//! `/mcp` call made with it becomes a child of that trace, so the OTLP
//! export shows one trace per run (OpenInference `TOOL` spans, sizes only).

use opentelemetry::Context;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
};
pub use tracing_opentelemetry::OpenTelemetrySpanExt;

/// A remote parent context for `trace_id_hex` (32 hex chars) with a span id
/// derived from `seed` (the gateway's request id): deterministic, so a
/// retried request lands on the same parent. `None` when the trace id is
/// not a valid OTel trace id.
#[must_use]
pub fn remote_run_context(trace_id_hex: &str, seed: &str) -> Option<Context> {
    let trace_id = TraceId::from_hex(trace_id_hex.trim()).ok()?;
    if trace_id == TraceId::INVALID {
        return None;
    }
    let digest = {
        use sha2::{Digest, Sha256};
        Sha256::digest(seed.as_bytes())
    };
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let span_id = SpanId::from_bytes(bytes);
    if span_id == SpanId::INVALID {
        return None;
    }
    let sc = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    Some(Context::new().with_remote_span_context(sc))
}

/// Make `span` a child of the run's trace. A no-op when no OTel layer is
/// installed or the trace id is invalid — the span is still a fine span.
pub fn attach_run_trace(span: &tracing::Span, trace_id_hex: &str, seed: &str) {
    if let Some(cx) = remote_run_context(trace_id_hex, seed) {
        let _ = span.set_parent(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_trace_id_yields_a_remote_sampled_parent_and_a_bad_one_none() {
        let cx = remote_run_context("0123456789abcdef0123456789abcdef", "req-1").expect("context");
        let sc = cx.span().span_context().clone();
        assert_eq!(
            sc.trace_id().to_string(),
            "0123456789abcdef0123456789abcdef"
        );
        assert!(sc.is_remote() && sc.is_sampled());
        let again = remote_run_context("0123456789abcdef0123456789abcdef", "req-1").unwrap();
        assert_eq!(
            again.span().span_context().span_id(),
            sc.span_id(),
            "deterministic per seed"
        );
        assert!(remote_run_context("not-hex", "x").is_none());
        assert!(remote_run_context("00000000000000000000000000000000", "x").is_none());
    }
}
