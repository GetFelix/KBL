//! Turning the exporter on from the standard `OTEL_*` variables.

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Flushes the last spans when dropped. Hold it for the life of the process.
#[must_use]
pub struct Guard(Option<SdkTracerProvider>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(provider) = self.0.take() {
            // Waits for the batch to leave; a collector that is down only costs the wait.
            let _ = provider.shutdown();
        }
    }
}

/// Starts exporting spans over OTLP/HTTP when an endpoint is configured
/// (`OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`),
/// and does nothing otherwise. `OTEL_SDK_DISABLED=true` and
/// `OTEL_TRACES_EXPORTER=none` switch it off even then. Headers, timeout,
/// sampler and resource attributes come from the other `OTEL_*` variables.
/// `service` is the `service.name` unless `OTEL_SERVICE_NAME` sets one.
///
/// Call it inside the Tokio runtime's process, once, before the first span.
pub fn init(service: &'static str) -> Guard {
    if !enabled(|k| std::env::var(k).ok()) {
        return Guard(None);
    }
    match build(service) {
        Ok(provider) => Guard(Some(provider)),
        // Tracing is a convenience: a bad exporter setting must not stop the service.
        Err(why) => {
            eprintln!("{service}: traces are off: {why}");
            Guard(None)
        }
    }
}

fn enabled(var: impl Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| var(k).is_some_and(|v| !v.trim().is_empty());
    let off = var("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
        || var("OTEL_TRACES_EXPORTER").is_some_and(|v| v.trim().eq_ignore_ascii_case("none"));
    !off && (set("OTEL_EXPORTER_OTLP_ENDPOINT") || set("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"))
}

fn build(service: &'static str) -> Result<SdkTracerProvider, Box<dyn std::error::Error>> {
    let exporter = SpanExporter::builder().with_http().build()?;
    // The SDK reads OTEL_SERVICE_NAME itself; ours is only the fallback.
    let mut resource = Resource::builder();
    if std::env::var_os("OTEL_SERVICE_NAME").is_none() {
        resource = resource.with_service_name(service);
    }
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.build())
        .build();
    global::set_text_map_propagator(TraceContextPropagator::new());
    tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer(service)))
        .try_init()?;
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::enabled;

    fn with<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn off_without_an_endpoint() {
        assert!(!enabled(with(&[])));
        assert!(!enabled(with(&[("OTEL_EXPORTER_OTLP_ENDPOINT", " ")])));
    }

    #[test]
    fn on_with_either_endpoint() {
        assert!(enabled(with(&[(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://c:4318"
        )])));
        assert!(enabled(with(&[(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "http://c:4318/v1/traces"
        )])));
    }

    #[test]
    fn the_standard_off_switches_win() {
        let url = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318");
        assert!(!enabled(with(&[url, ("OTEL_SDK_DISABLED", "true")])));
        assert!(!enabled(with(&[url, ("OTEL_TRACES_EXPORTER", "none")])));
    }
}
