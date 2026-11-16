//! `prices import`: writes `price_set` records from a price table, skipping
//! prices that are already in effect. See docs/work-tracker.md, "Cost and spend".

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use clap::Args;
use kbl_core::work::PriceUnit;
use kbl_core::work_fold::{PriceInEffect, model_name_ok};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::felix::Session;

/// The table shipped with this build: `kbl-tool prices import --default`.
const DEFAULT_TABLE: &str = include_str!("../prices/default.json");

#[derive(Args)]
pub(crate) struct ImportArgs {
    /// A price table (JSON). See the default table for the shape.
    #[arg(required_unless_present = "default", conflicts_with = "default")]
    file: Option<String>,
    /// Import the table shipped with this build.
    #[arg(long)]
    default: bool,
    /// Also write a price that differs from the one in effect. Without it
    /// the difference is listed and left alone.
    #[arg(long)]
    update: bool,
    /// Say what would be written and write nothing.
    #[arg(long)]
    dry_run: bool,
    #[arg(long, env = "KBL_TRACKER_URL", default_value = "http://127.0.0.1:8790")]
    tracker_url: String,
    /// A person's tracker token. Only a person may set a price.
    #[arg(long, env = "KBL_TRACKER_TOKEN", hide_env_values = true)]
    tracker_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Table {
    /// The day a person copied the prices from the vendor.
    pub(crate) as_of: String,
    /// Where they came from.
    pub(crate) source: String,
    pub(crate) prices: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) model: String,
    pub(crate) unit: PriceUnit,
    pub(crate) price_micros: u64,
    /// `YYYY-MM-DD` (UTC). A day that has not come yet is sent as a price for
    /// later; a day that has passed means now, since a price cannot reach back.
    #[serde(default)]
    pub(crate) effective_from: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing is in effect for this model and unit.
    Write,
    /// The same price is already in effect.
    Skip,
    /// A different price is in effect.
    Differs { in_effect: u64 },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Planned {
    pub(crate) model: String,
    pub(crate) unit: PriceUnit,
    pub(crate) price_micros: u64,
    /// Set only for a day in the future.
    pub(crate) effective_from_ms: Option<u64>,
    pub(crate) step: Step,
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` as milliseconds at 00:00 UTC.
pub(crate) fn day_ms(day: &str) -> Result<u64> {
    let bad = || anyhow::anyhow!("`{day}` is not a date: write YYYY-MM-DD");
    let parts: Vec<&str> = day.split('-').collect();
    let [y, m, d] = parts[..] else {
        return Err(bad());
    };
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return Err(bad());
    }
    let (y, m, d): (i64, i64, i64) = (
        y.parse().map_err(|_| bad())?,
        m.parse().map_err(|_| bad())?,
        d.parse().map_err(|_| bad())?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    let days = days_from_civil(y, m, d);
    // A day that rolls over (such as 02-31) is not that day.
    if days_from_civil(y, m, 1) + d - 1 != days
        || (m < 12 && days >= days_from_civil(y, m + 1, 1))
        || (m == 12 && days >= days_from_civil(y + 1, 1, 1))
    {
        return Err(bad());
    }
    u64::try_from(days * 86_400_000).map_err(|_| bad())
}

pub(crate) fn parse(text: &str) -> Result<Table> {
    let table: Table = serde_json::from_str(text).context("the price table is not valid")?;
    day_ms(&table.as_of).context("as_of")?;
    if table.source.trim().is_empty() {
        bail!("the price table has no source: say where the prices came from");
    }
    let mut seen = BTreeSet::new();
    for e in &table.prices {
        if !model_name_ok(&e.model) {
            bail!(
                "`{}` is not a model name: letters, digits and `.-_:/@`, up to 100",
                e.model
            );
        }
        if let Some(day) = &e.effective_from {
            day_ms(day).with_context(|| format!("{} {:?}", e.model, e.unit))?;
        }
        if !seen.insert((e.model.as_str(), e.unit, e.effective_from.as_deref())) {
            bail!("{} {:?} is listed twice for the same day", e.model, e.unit);
        }
    }
    Ok(table)
}

/// A day this close to now (or past) is written as now. The tracker refuses a
/// price that starts before its own clock reads, and the two clocks differ.
const NOW_MARGIN_MS: u64 = 10 * 60 * 1000;

/// What importing `table` would do. `in_effect_at(ms)` is the prices in effect
/// at `ms`: a price for later is compared with what will be in effect on its
/// day, so planning it again after it was written finds it already there.
pub(crate) fn plan(
    table: &Table,
    in_effect_at: &dyn Fn(u64) -> Vec<PriceInEffect>,
    now_ms: u64,
) -> Result<Vec<Planned>> {
    table
        .prices
        .iter()
        .map(|e| {
            let from = match &e.effective_from {
                Some(day) => Some(day_ms(day)?).filter(|ms| *ms > now_ms + NOW_MARGIN_MS),
                None => None,
            };
            let current = in_effect_at(from.unwrap_or(now_ms))
                .iter()
                .find(|p| p.model == e.model && p.unit == e.unit)
                .map(|p| p.price_micros);
            let step = match current {
                None => Step::Write,
                Some(p) if p == e.price_micros => Step::Skip,
                Some(p) => Step::Differs { in_effect: p },
            };
            Ok(Planned {
                model: e.model.clone(),
                unit: e.unit,
                price_micros: e.price_micros,
                effective_from_ms: from,
                step,
            })
        })
        .collect()
}

fn unit_name(unit: PriceUnit) -> String {
    serde_json::to_value(unit)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The `POST /v1/work/prices` body for one planned price.
pub(crate) fn body(p: &Planned) -> Value {
    let mut b = json!({ "model": p.model, "unit": p.unit, "price_micros": p.price_micros });
    if let Some(ms) = p.effective_from_ms {
        b["effective_from_ms"] = json!(ms);
    }
    b
}

/// Whether `update` lets this step be written.
pub(crate) fn writes(p: &Planned, update: bool) -> bool {
    match p.step {
        Step::Write => true,
        Step::Differs { .. } => update,
        Step::Skip => false,
    }
}

pub(crate) async fn import(target: crate::felix::Target, args: ImportArgs) -> Result<()> {
    let text = match &args.file {
        Some(path) => std::fs::read_to_string(path).with_context(|| format!("read {path}"))?,
        None => DEFAULT_TABLE.to_owned(),
    };
    let table = parse(&text)?;
    let token = match (&args.tracker_token, args.dry_run) {
        (Some(t), _) => Some(t.clone()),
        (None, true) => None,
        (None, false) => {
            bail!("a person's tracker token is needed: --tracker-token or KBL_TRACKER_TOKEN")
        }
    };

    let session = Session::sign_in(target).await?;
    let records = session
        .broker()
        .await?
        .read_all(kbl_core::work::WORK_EVENTS)
        .await?;
    let now = crate::now_ms();
    let state = crate::fold_log(&records)?;
    let planned = plan(&table, &|at| state.prices_in_effect(at), now)?;

    println!("prices as of {} ({})", table.as_of, table.source);
    let (mut wrote, mut skipped, mut differs) = (0, 0, 0);
    for p in &planned {
        let name = format!("{} {}", p.model, unit_name(p.unit));
        match p.step {
            Step::Skip => {
                skipped += 1;
                println!("  skip    {name}: already {}", p.price_micros);
            }
            Step::Differs { in_effect } if !writes(p, args.update) => {
                differs += 1;
                println!(
                    "  differs {name}: {in_effect} in effect, table has {} (--update writes it)",
                    p.price_micros
                );
            }
            _ if args.dry_run => {
                wrote += 1;
                println!("  would write {name}: {}", p.price_micros);
            }
            _ => {
                let Some(token) = &token else { continue };
                post(&session, &args.tracker_url, token, p).await?;
                wrote += 1;
                println!("  wrote   {name}: {}", p.price_micros);
            }
        }
    }
    let verb = if args.dry_run { "would write" } else { "wrote" };
    println!("{verb} {wrote}, skipped {skipped}, left {differs} that differ");
    Ok(())
}

async fn post(session: &Session, url: &str, token: &str, p: &Planned) -> Result<()> {
    let url = format!("{}/v1/work/prices", url.trim_end_matches('/'));
    let response = session
        .http()
        .post(url)
        .bearer_auth(token)
        .json(&body(p))
        .send()
        .await
        .context("could not reach the tracker")?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body: Value = response.json().await.unwrap_or(Value::Null);
    let error = body["error"].as_str().unwrap_or("no reason given");
    let why = body["why"]
        .as_str()
        .map(|w| format!(" ({w})"))
        .unwrap_or_default();
    bail!(
        "the tracker refused {} {:?}: {status}: {error}{why}. Prices before this one were written; run the import again to finish",
        p.model,
        p.unit
    )
}

#[cfg(test)]
mod tests;
