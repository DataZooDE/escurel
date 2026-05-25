//! Custom JSON event formatter emitting the substrate log contract.
//!
//! The off-the-shelf `tracing_subscriber` JSON formatter names its
//! fields `timestamp`/`level`/`fields.message` and offers no hook to
//! stamp constant `app`/`env`/`version` keys or to flatten span fields
//! (e.g. `request_id`) onto the top-level record. The substrate audit
//! contract (see `docs/spec/platform.md` §Observability) requires
//! exactly `ts`, `level`, `msg`, plus `app`/`env`/`version` and any
//! span fields hoisted to the top level — so we format events
//! ourselves.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Per-event/per-span field collector. Captures the special `message`
/// field separately so it can be emitted as `msg`.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: Map<String, Value>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(rendered);
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(rendered));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(value.to_string()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), Value::Bool(value));
    }
}

/// Constant fields stamped onto every record.
#[derive(Clone)]
pub(crate) struct StaticFields {
    pub app: String,
    pub env: String,
    pub version: String,
}

/// `FormatEvent` that renders the substrate JSON log contract.
pub(crate) struct JsonContract {
    pub statics: StaticFields,
}

impl<S, N> FormatEvent<S, N> for JsonContract
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();

        // Collect span fields first (outermost → innermost) so that
        // inner spans and the event itself win on key collisions, and
        // so identifiers like `request_id` are hoisted top-level.
        let mut span_fields: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<SpanFields>() {
                    for (k, v) in &fields.0 {
                        span_fields.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let mut record = Map::new();
        record.insert(
            "ts".to_string(),
            Value::String(
                time::OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
            ),
        );
        record.insert(
            "level".to_string(),
            Value::String(meta.level().as_str().to_ascii_lowercase()),
        );
        record.insert("app".to_string(), Value::String(self.statics.app.clone()));
        record.insert("env".to_string(), Value::String(self.statics.env.clone()));
        record.insert(
            "version".to_string(),
            Value::String(self.statics.version.clone()),
        );

        // Span fields, then event fields (event wins).
        for (k, v) in span_fields {
            record.insert(k, v);
        }
        if let Some(msg) = visitor.message {
            record.insert("msg".to_string(), Value::String(msg));
        }
        for (k, v) in visitor.fields {
            record.insert(k, v);
        }
        record
            .entry("target".to_string())
            .or_insert_with(|| Value::String(meta.target().to_string()));

        let line = serde_json::to_string(&Value::Object(record)).map_err(|_| fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

/// Span fields captured at span creation, stashed in the span's
/// extensions for later lookup by [`JsonContract`].
pub(crate) struct SpanFields(pub Map<String, Value>);

/// A `Layer` that records a span's own fields into its extensions so
/// they can be flattened onto child events.
pub(crate) struct SpanFieldLayer;

impl<S> tracing_subscriber::Layer<S> for SpanFieldLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        if let Some(msg) = visitor.message {
            visitor.fields.insert("msg".to_string(), Value::String(msg));
        }
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanFields(visitor.fields));
        }
    }
}
