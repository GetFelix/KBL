//! An in-memory exporter for tests. It scopes the subscriber to the calling
//! thread, so use it on a current-thread runtime (the `#[tokio::test]` default).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing::Dispatch;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt;

/// A finished span, reduced to what a test asserts on.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    /// All zeros for a root.
    pub parent_id: String,
    pub attributes: BTreeMap<String, String>,
}

/// One capture at a time: two scoped subscribers coming and going on parallel
/// test threads make tracing's per-callsite cache drop spans.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Two subscribers that live for the whole process. With only one registered,
/// tracing asks just the registering thread's subscriber about a new callsite,
/// so a parallel test thread with none caches the callsite as disabled for us.
static KEEP: OnceLock<[Dispatch; 2]> = OnceLock::new();

/// Records every span closed while it is alive.
pub struct Capture {
    _serial: MutexGuard<'static, ()>,
    exporter: InMemorySpanExporter,
    provider: SdkTracerProvider,
    _scope: DefaultGuard,
}

/// Installs the trace-context propagator and a subscriber for this thread.
pub fn capture() -> Capture {
    let serial = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    KEEP.get_or_init(|| {
        [
            Dispatch::new(tracing_subscriber::registry()),
            Dispatch::new(tracing_subscriber::registry()),
        ]
    });
    global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let scope = tracing::subscriber::set_default(subscriber);
    // A callsite already cached as disabled has to look again.
    tracing_core::callsite::rebuild_interest_cache();
    Capture {
        _serial: serial,
        exporter,
        provider,
        _scope: scope,
    }
}

impl Capture {
    /// The spans finished so far, oldest first.
    pub fn finished(&self) -> Vec<Recorded> {
        let _ = self.provider.force_flush();
        self.exporter
            .get_finished_spans()
            .unwrap_or_default()
            .into_iter()
            .map(|s| Recorded {
                name: s.name.to_string(),
                trace_id: s.span_context.trace_id().to_string(),
                span_id: s.span_context.span_id().to_string(),
                parent_id: s.parent_span_id.to_string(),
                attributes: s
                    .attributes
                    .iter()
                    .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                    .collect(),
            })
            .collect()
    }
}
