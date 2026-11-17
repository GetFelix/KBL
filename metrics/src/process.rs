//! Process basics: start time, uptime, build info, and on Linux memory, CPU
//! and open files read from `/proc`.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static STARTED: OnceLock<Instant> = OnceLock::new();

pub(crate) fn start(service: &'static str) {
    STARTED.get_or_init(Instant::now);
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    metrics::gauge!("process_start_time_seconds").set(at);
    metrics::gauge!(
        "kbl_build_info",
        "service" => service,
        "version" => env!("CARGO_PKG_VERSION")
    )
    .set(1.0);
}

pub(crate) fn refresh() {
    if let Some(started) = STARTED.get() {
        metrics::gauge!("kbl_uptime_seconds").set(started.elapsed().as_secs_f64());
    }
    #[cfg(target_os = "linux")]
    linux();
}

#[cfg(target_os = "linux")]
fn linux() {
    // The kernel's clock tick and page size on every platform Linux builds run on.
    const TICKS: f64 = 100.0;
    const PAGE: f64 = 4096.0;
    if let Ok(stat) = std::fs::read_to_string("/proc/self/stat")
        // The command name is in parentheses and may hold spaces: read after it.
        && let Some((_, rest)) = stat.rsplit_once(')')
    {
        let f: Vec<&str> = rest.split_whitespace().collect();
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
        if let (Some(u), Some(s)) = (num(11), num(12)) {
            // Whole seconds: a Prometheus counter is an integer.
            metrics::counter!("process_cpu_seconds_total").absolute(((u + s) / TICKS) as u64);
        }
        if let Some(pages) = num(21) {
            metrics::gauge!("process_resident_memory_bytes").set(pages * PAGE);
        }
    }
    if let Ok(dir) = std::fs::read_dir("/proc/self/fd") {
        metrics::gauge!("process_open_fds").set(dir.count() as f64);
    }
}
