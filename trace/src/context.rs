//! W3C `traceparent` between services that talk over HTTP.

use http::HeaderMap;
use opentelemetry::global;
use opentelemetry_http::{HeaderExtractor, HeaderInjector};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// The trace headers for a call made inside the current span. Empty when
/// tracing is off, so a caller can attach it unconditionally.
pub fn headers() -> HeaderMap {
    let mut out = HeaderMap::new();
    let cx = Span::current().context();
    global::get_text_map_propagator(|p| p.inject_context(&cx, &mut HeaderInjector(&mut out)));
    out
}

/// Makes `span` a child of the caller named in `incoming`, if it names one.
pub fn adopt_parent(span: &Span, incoming: &HeaderMap) {
    let cx = global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(incoming)));
    // A request with no valid traceparent leaves the span a root.
    let _ = span.set_parent(cx);
}
