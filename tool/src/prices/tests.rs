use kbl_core::work_fold::PriceInEffect;

use super::*;

fn table(prices: &str) -> Table {
    parse(&format!(
        r#"{{"as_of":"2026-11-15","source":"vendor page","prices":[{prices}]}}"#
    ))
    .unwrap()
}

fn in_effect(model: &str, unit: PriceUnit, price_micros: u64) -> PriceInEffect {
    PriceInEffect {
        model: model.into(),
        unit,
        price_micros,
        since_ms: 0,
    }
}

const NOW: u64 = 1_800_000_000_000;

#[test]
fn the_shipped_table_is_valid_and_dated() {
    let t = parse(DEFAULT_TABLE).unwrap();
    assert!(!t.prices.is_empty());
    assert!(t.prices.iter().all(|e| e.price_micros > 0));
    // Every priced model has both sides of a normal report.
    for e in t.prices.iter().filter(|e| e.unit == PriceUnit::Input) {
        assert!(
            t.prices
                .iter()
                .any(|o| o.model == e.model && o.unit == PriceUnit::Output),
            "{} has no output price",
            e.model
        );
    }
}

#[test]
fn a_price_in_effect_is_skipped_a_new_one_written_a_different_one_listed() {
    let t = table(
        r#"{"model":"a","unit":"input","price_micros":10},
           {"model":"a","unit":"output","price_micros":20},
           {"model":"b","unit":"input","price_micros":30}"#,
    );
    let have = [
        in_effect("a", PriceUnit::Input, 10),
        in_effect("a", PriceUnit::Output, 99),
    ];
    let steps: Vec<_> = plan(&t, &|_| have.to_vec(), NOW)
        .unwrap()
        .into_iter()
        .map(|p| p.step)
        .collect();
    assert_eq!(
        steps,
        [Step::Skip, Step::Differs { in_effect: 99 }, Step::Write]
    );
}

#[test]
fn a_differing_price_is_written_only_with_update() {
    let t = table(r#"{"model":"a","unit":"input","price_micros":10}"#);
    let p = &plan(&t, &|_| vec![in_effect("a", PriceUnit::Input, 5)], NOW).unwrap()[0];
    assert!(!writes(p, false));
    assert!(writes(p, true));
}

#[test]
fn only_a_future_day_is_sent_as_effective_from() {
    let t = table(
        r#"{"model":"a","unit":"input","price_micros":1,"effective_from":"2020-01-01"},
           {"model":"b","unit":"input","price_micros":1,"effective_from":"2099-01-01"}"#,
    );
    let p = plan(&t, &|_| vec![], NOW).unwrap();
    assert_eq!(
        body(&p[0]),
        json!({"model":"a","unit":"input","price_micros":1,"as_of":"2026-11-15"})
    );
    assert_eq!(
        body(&p[1]),
        json!({"model":"b","unit":"input","price_micros":1,"as_of":"2026-11-15","effective_from_ms":4_070_908_800_000u64})
    );
}

#[test]
fn days_parse_to_utc_midnight_and_bad_ones_are_refused() {
    assert_eq!(day_ms("1970-01-02").unwrap(), 86_400_000);
    assert_eq!(day_ms("2024-02-29").unwrap(), 1_709_164_800_000);
    for bad in [
        "2026-02-30",
        "2026-13-01",
        "2026-1-1",
        "tomorrow",
        "2026-00-10",
    ] {
        assert!(day_ms(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_bad_table_is_refused() {
    let wrap = |p: &str, source: &str| {
        parse(&format!(
            r#"{{"as_of":"2026-11-15","source":"{source}","prices":[{p}]}}"#
        ))
    };
    assert!(
        wrap(
            r#"{"model":"bad name","unit":"input","price_micros":1}"#,
            "x"
        )
        .is_err()
    );
    assert!(wrap(r#"{"model":"a","unit":"bogus","price_micros":1}"#, "x").is_err());
    assert!(wrap(r#"{"model":"a","unit":"input","price_micros":1}"#, " ").is_err());
    assert!(
        wrap(
            r#"{"model":"a","unit":"input","price_micros":1},{"model":"a","unit":"input","price_micros":2}"#,
            "x"
        )
        .is_err()
    );
    assert!(
        wrap(
            r#"{"model":"a","unit":"input","price_micros":1,"extra":1}"#,
            "x"
        )
        .is_err()
    );
}

#[test]
fn a_future_price_already_scheduled_is_skipped_on_the_next_run() {
    let t = table(r#"{"model":"a","unit":"input","price_micros":7,"effective_from":"2099-01-01"}"#);
    let day = day_ms("2099-01-01").unwrap();
    // The price is not in effect now (5 is), but it is on its own day.
    let at = |ms: u64| {
        let price = if ms >= day { 7 } else { 5 };
        vec![in_effect("a", PriceUnit::Input, price)]
    };
    let p = plan(&t, &at, NOW).unwrap();
    assert_eq!(p[0].effective_from_ms, Some(day));
    assert_eq!(p[0].step, Step::Skip);
    assert!(!writes(&p[0], true));
}

#[test]
fn a_day_just_ahead_of_the_clock_is_written_as_now() {
    let t = table(r#"{"model":"a","unit":"input","price_micros":1,"effective_from":"2026-11-16"}"#);
    let midnight = day_ms("2026-11-16").unwrap();
    let p = plan(&t, &|_| vec![], midnight - 60_000).unwrap();
    assert_eq!(p[0].effective_from_ms, None);
    let p = plan(&t, &|_| vec![], midnight - 3_600_000).unwrap();
    assert_eq!(p[0].effective_from_ms, Some(midnight));
}

#[test]
fn the_shipped_table_says_haiku_is_a_tiered_price() {
    let t = parse(DEFAULT_TABLE).unwrap();
    assert!(t.source.contains("100K"), "{}", t.source);
    for m in [
        "claude-fable-5-1",
        "claude-fable-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
    ] {
        assert!(t.prices.iter().any(|e| e.model == m), "{m}");
    }
}
