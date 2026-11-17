//! Ingest's metrics. The `source` label is the adapter's name (`github`),
//! never a repo, issue or login. See docs/metrics.md.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::source::SourceError;

/// A poll ended: its count by result, its duration, and when the last good one finished.
pub(crate) fn poll_finished(
    source: &'static str,
    took: Duration,
    result: &Result<(), SourceError>,
) {
    let (outcome, error) = match result {
        Ok(()) => ("ok", None),
        Err(SourceError::RateLimited { .. }) => ("rate_limited", Some("rate_limited")),
        Err(SourceError::Failed(_)) => ("failed", Some("failed")),
    };
    metrics::counter!("kbl_ingest_polls_total", "source" => source, "result" => outcome)
        .increment(1);
    metrics::histogram!("kbl_ingest_poll_seconds", "source" => source).record(took.as_secs_f64());
    if let Some(kind) = error {
        metrics::counter!("kbl_ingest_errors_total", "source" => source, "kind" => kind)
            .increment(1);
    } else if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        metrics::gauge!("kbl_ingest_last_success_timestamp_seconds", "source" => source)
            .set(now.as_secs_f64());
    }
}

/// An upstream change was recorded: the time since upstream stamped it.
pub(crate) fn recorded(source: &'static str, updated_at: &str) {
    let Some(at) = epoch_seconds(updated_at) else {
        return;
    };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    let lag = now.as_secs_f64() - at as f64;
    metrics::histogram!("kbl_ingest_lag_seconds", "source" => source).record(lag.max(0.0));
}

/// Repos the source could not read this poll.
pub(crate) fn denied(source: &'static str, repos: usize) {
    metrics::gauge!("kbl_ingest_denied_repos", "source" => source).set(repos as f64);
}

/// A webhook delivery and what became of it.
pub(crate) fn webhook(result: &'static str) {
    metrics::counter!("kbl_ingest_webhooks_total", "result" => result).increment(1);
}

/// Seconds since the epoch of a UTC timestamp like `2026-11-10T10:00:00Z`.
fn epoch_seconds(ts: &str) -> Option<i64> {
    let ts = ts.strip_suffix('Z')?;
    let (date, time) = ts.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':');
    let (h, min) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    let sec = t.next()?.split('.').next()?.parse::<i64>().ok()?;
    // Days from the civil date, as in Howard Hinnant's algorithm.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + min * 60 + sec)
}

#[cfg(test)]
mod tests;
