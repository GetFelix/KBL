//! Prometheus metrics for every KBL service. A service installs the recorder
//! once, records through the `metrics` macros, and serves [`Exporter::render`]
//! on its own listener. See docs/metrics.md for every metric and label.
//!
//! Labels are route templates, tool names, source kinds and result words,
//! never item text, names, ids or tokens: a scrape must be safe to hand to
//! anyone who can reach the port.

mod process;
mod serve;

use std::sync::OnceLock;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

pub use serve::{check_listen, router, serve};

/// Latency buckets, in seconds, for every histogram not named `*_lag_seconds`.
const LATENCY_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];
/// Lag buckets, in seconds: from one poll interval to a day.
const LAG_BUCKETS: &[f64] = &[
    1.0, 5.0, 15.0, 60.0, 300.0, 900.0, 3600.0, 21_600.0, 86_400.0,
];

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Renders what the process has recorded.
#[derive(Clone)]
pub struct Exporter(PrometheusHandle);

/// Installs the process-wide recorder, once. A second call returns the same
/// exporter, so tests in one process share it.
pub fn install(service: &'static str) -> Exporter {
    let handle = HANDLE.get_or_init(|| {
        let recorder = PrometheusBuilder::new()
            .set_buckets_for_metric(Matcher::Suffix("lag_seconds".into()), LAG_BUCKETS)
            .and_then(|b| b.set_buckets(LATENCY_BUCKETS))
            .expect("the bucket lists are not empty")
            .build_recorder();
        let handle = recorder.handle();
        if metrics::set_global_recorder(recorder).is_err() {
            eprintln!("{service}: another metrics recorder was installed first; /metrics is empty");
        }
        process::start(service);
        handle
    });
    Exporter(handle.clone())
}

impl Exporter {
    /// The Prometheus text exposition, with the process gauges brought up to date.
    pub fn render(&self) -> String {
        process::refresh();
        self.0.render()
    }
}

#[cfg(test)]
mod tests;
