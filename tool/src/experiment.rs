//! `work metrics`: the Metrics tab's numbers for a time window, written as JSON or CSV so
//! each arm of the supervision experiment can be archived. See
//! docs/design/supervision-experiment.md.
//!
//! It only reads `work.events` and folds it with the dashboard's own fold. It writes
//! nothing to Felix or the tracker.

use anyhow::{Result, bail, ensure};
use clap::{Args, ValueEnum};
use kbl_wasm::Folder;
use serde_json::{Value, json};

use crate::felix::{Session, Target};

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Format {
    Json,
    Csv,
}

#[derive(Args)]
pub(crate) struct MetricsArgs {
    /// Names this run in the output, such as `arm-kbl-4`.
    #[arg(long)]
    label: String,
    /// Window length in days, 1 to 90, ending at `--to-ms`.
    #[arg(long, default_value_t = 7)]
    days: u32,
    /// End of the window in epoch milliseconds. Default: now.
    #[arg(long)]
    to_ms: Option<u64>,
    /// Engineer minutes logged by hand for the window.
    #[arg(long)]
    engineer_minutes: Option<f64>,
    /// Merged fixes in the window, from GitHub. The Metrics tab's done count also holds
    /// reviews, triage and redo follow-ups, so minutes per fix divide by this.
    #[arg(long)]
    merged_fixes: Option<u64>,
    #[arg(long, value_enum, default_value = "json")]
    format: Format,
}

pub(crate) async fn metrics(target: Target, args: MetricsArgs) -> Result<()> {
    let broker = Session::sign_in(target).await?.broker().await?;
    let records = broker.read_all(kbl_core::work::WORK_EVENTS).await?;
    let to_ms = args.to_ms.unwrap_or_else(crate::now_ms);
    let out = export(
        &records,
        &Window {
            label: &args.label,
            days: args.days,
            to_ms,
            explicit_end: args.to_ms.is_some(),
            engineer_minutes: args.engineer_minutes,
            merged_fixes: args.merged_fixes,
        },
    )?;
    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&out)?),
        Format::Csv => print!("{}", csv(&out)),
    }
    Ok(())
}

/// What one export covers.
pub(crate) struct Window<'a> {
    pub(crate) label: &'a str,
    pub(crate) days: u32,
    pub(crate) to_ms: u64,
    /// `to_ms` was asked for, not defaulted to now.
    pub(crate) explicit_end: bool,
    pub(crate) engineer_minutes: Option<f64>,
    pub(crate) merged_fixes: Option<u64>,
}

/// The report over `records` for the window, with the headline figures flat.
pub(crate) fn export(records: &[(u64, Vec<u8>)], w: &Window) -> Result<Value> {
    let Window {
        label,
        days,
        to_ms,
        engineer_minutes,
        ..
    } = *w;
    ensure!((1..=90).contains(&days), "--days is 1 to 90");
    let mut fold = Folder::new();
    // Refused and malformed records are not applied; the count shows a log that is mostly noise.
    let mut not_applied = 0u64;
    for (offset, payload) in records {
        if !fold.apply(*offset as f64, payload) {
            not_applied += 1;
        }
    }
    if fold.newer() > 0 {
        bail!("work.events holds a record newer than this build reads; update kbl-tool");
    }
    let report: Value = serde_json::from_str(&fold.metrics_json(to_ms as f64, days))?;
    // The fold moves the window's end up to the newest record, so a window ending
    // earlier would silently report a later block.
    if w.explicit_end && report["clock_behind"] == true {
        bail!(
            "work.events has records after --to-ms; the fold only keeps each item's latest done time, so an earlier window cannot be rebuilt. Export at the end of each block."
        );
    }
    let done = report["throughput"]["done"].as_u64().unwrap_or(0);
    let per_fix_minutes = engineer_minutes
        .zip(w.merged_fixes.filter(|n| *n > 0))
        .map(|(m, n)| m / n as f64);
    let unpriced = report["cost"]["by_model"]
        .as_array()
        .is_some_and(|m| m.iter().any(|x| x["unpriced"] == true));
    let summary = json!({
        "done": done,
        "done_per_day": done as f64 / f64::from(days),
        "touched_share_permille": report["supervision"]["touched_share_permille"],
        "touches_per_done": report["supervision"]["touches_per_done"],
        "wait_on_person_p50_ms": report["supervision"]["wait_on_person"]["p50_ms"],
        "wait_on_person_p85_ms": report["supervision"]["wait_on_person"]["p85_ms"],
        "redos_per_done": report["rework"]["redos_per_done"],
        "reopened_per_done": report["rework"]["reopened_per_done"],
        "failed_attempts_per_done": report["rework"]["failed_attempts_per_done"],
        "cost_per_done_micros": report["cost"]["per_done_micros"],
        "engineer_minutes": engineer_minutes,
        "merged_fixes": w.merged_fixes,
        "engineer_minutes_per_merged_fix": per_fix_minutes,
        "cost_is_a_floor": unpriced,
    });
    Ok(json!({
        "label": label,
        "records": records.len(),
        "records_not_applied": not_applied,
        "clock_behind": report["clock_behind"],
        "partial": report["partial"],
        "summary": summary,
        "report": report,
    }))
}

/// `name,value` rows of the label, window and summary; an absent figure is an empty cell.
fn csv(out: &Value) -> String {
    let mut s = String::from("name,value\n");
    let mut row = |k: &str, v: &Value| {
        let cell = match v {
            Value::Null => String::new(),
            Value::String(t) => t.clone(),
            other => other.to_string(),
        };
        s.push_str(&format!("{k},{cell}\n"));
    };
    row("label", &out["label"]);
    row("from_ms", &out["report"]["from_ms"]);
    row("to_ms", &out["report"]["to_ms"]);
    row("clock_behind", &out["clock_behind"]);
    row("partial", &out["partial"]);
    for (k, v) in out["summary"].as_object().into_iter().flatten() {
        row(k, v);
    }
    s
}

#[cfg(test)]
mod tests;
