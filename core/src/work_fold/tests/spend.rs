//! Spend from format 8: usage per model, priced from `price_set` records.

use super::*;
use crate::work::{PriceUnit, Usage};

fn tokens(model: &str, input: u64, output: u64) -> Usage {
    Usage::Tokens {
        model: model.into(),
        input,
        output,
        cache_read: 0,
        cache_write: 0,
    }
}

fn price(model: &str, unit: PriceUnit, price_micros: u64) -> WorkRecord {
    WorkRecord::PriceSet {
        model: model.into(),
        unit,
        price_micros,
        effective_from_ms: None,
        as_of: None,
    }
}

fn done_with(item: &str, usage: Vec<Usage>, floor_micros: u64) -> WorkRecord {
    WorkRecord::Done {
        item: id(item),
        attempt: 1,
        cost_micros: 0,
        usage,
        floor_micros,
    }
}

/// `held_root` on a log raised to format 9, where a price may carry `as_of`.
fn raised9() -> Log {
    let mut l = held_root();
    l.person(WorkRecord::FormatRaised { to: 9 });
    l
}

/// `held_root` on a log raised to format 8.
fn raised() -> Log {
    let mut l = held_root();
    l.person(WorkRecord::FormatRaised { to: 8 });
    l
}

fn rejected(s: &State) -> Vec<Reject> {
    s.rejections.iter().map(|r| r.why).collect()
}

#[test]
fn usage_is_priced_per_unit_and_drawn_from_the_budget() {
    let mut l = raised();
    l.person(price("m", PriceUnit::Input, 3_000_000));
    l.person(price("m", PriceUnit::Output, 15_000_000));
    l.agent("w1", done_with("r1", vec![tokens("m", 20_000, 4_000)], 0));
    let s = fold(&l.0);
    assert_eq!(rejected(&s), []);
    // 20k input at $3/M plus 4k output at $15/M.
    assert_eq!(s.items[&id("r1")].cost_micros, 60_000 + 60_000);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 1_000_000 - 120_000);
    let spend = s.spend();
    let m = &spend.by_model["m"];
    assert_eq!(
        (m.input, m.output, m.cost_micros, m.unpriced),
        (20_000, 4_000, 120_000, false)
    );
    assert_eq!(spend.total_micros, 120_000);
}

#[test]
fn a_report_is_charged_at_least_its_floor() {
    let mut l = raised();
    l.person(price("m", PriceUnit::Input, 1_000_000));
    l.agent("w1", done_with("r1", vec![tokens("m", 10, 0)], 100_000));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 100_000);
    assert_eq!(s.spend().by_model["m"].cost_micros, 10);
}

/// A later price applies to later usage only: history keeps the price it was logged under.
#[test]
fn a_price_change_does_not_reprice_history() {
    let mut l = raised();
    l.person(price("m", PriceUnit::Input, 1_000_000));
    l.agent("w1", done_with("r1", vec![tokens("m", 100_000, 0)], 0));
    l.person(price("m", PriceUnit::Input, 9_000_000));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 100_000);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 900_000);
}

#[test]
fn a_price_set_for_later_leaves_usage_before_it_on_the_old_price() {
    let mut l = raised();
    l.person(price("m", PriceUnit::Input, 1_000_000));
    l.person(WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Input,
        price_micros: 9_000_000,
        effective_from_ms: Some(1_000_000),
        as_of: None,
    });
    l.agent("w1", done_with("r1", vec![tokens("m", 100_000, 0)], 0));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 100_000);
}

/// An unknown model is recorded and charged the floor, never guessed. A price
/// set later draws the rest of what its usage cost.
#[test]
fn unpriced_usage_costs_the_floor_until_a_price_comes() {
    let mut l = raised();
    l.agent(
        "w1",
        done_with("r1", vec![tokens("new", 100_000, 0)], 50_000),
    );
    let s = fold(&l.0);
    assert_eq!(rejected(&s), []);
    assert_eq!(s.items[&id("r1")].cost_micros, 50_000);
    assert_eq!(s.unpriced_models().into_iter().collect::<Vec<_>>(), ["new"]);
    let spend = s.spend();
    assert!(spend.by_model["new"].unpriced);
    assert_eq!(
        spend
            .unpriced
            .iter()
            .map(|u| u.model.as_str())
            .collect::<Vec<_>>(),
        ["new"]
    );

    l.person(price("new", PriceUnit::Input, 2_000_000));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 200_000);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 800_000);
    assert!(s.unpriced_models().is_empty());
    let m = &s.spend().by_model["new"];
    assert_eq!((m.cost_micros, m.unpriced), (200_000, false));
}

/// Pricing one unit of a report leaves it waiting on the others.
#[test]
fn a_report_waits_until_every_unit_it_used_is_priced() {
    let mut l = raised();
    l.agent(
        "w1",
        done_with("r1", vec![tokens("new", 100_000, 10_000)], 0),
    );
    l.person(price("new", PriceUnit::Input, 1_000_000));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 100_000);
    assert!(s.spend().by_model["new"].unpriced);
    l.person(price("new", PriceUnit::Output, 10_000_000));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 200_000);
    assert!(!s.spend().by_model["new"].unpriced);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 800_000);
}

#[test]
fn aic_is_priced_per_credit() {
    let mut l = raised();
    l.person(price("c", PriceUnit::Aic, 40_000));
    l.agent(
        "w1",
        done_with(
            "r1",
            vec![Usage::Aic {
                model: "c".into(),
                milli: 2_500,
            }],
            0,
        ),
    );
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 100_000);
    assert_eq!(s.spend().by_model["c"].aic_milli, 2_500);
}

#[test]
fn from_format_8_a_reported_cost_is_refused() {
    let mut l = raised();
    l.agent(
        "w1",
        WorkRecord::Done {
            item: id("r1"),
            attempt: 1,
            cost_micros: 1,
            usage: Vec::new(),
            floor_micros: 0,
        },
    );
    let s = fold(&l.0);
    assert_eq!(rejected(&s), [Reject::BadUsage]);
    assert!(s.items[&id("r1")].hold.is_some());
}

#[test]
fn a_model_twice_or_a_bad_name_is_refused() {
    for usage in [
        vec![tokens("m", 1, 0), tokens("m", 1, 0)],
        vec![tokens("", 1, 0)],
        vec![tokens("has space", 1, 0)],
        vec![tokens(&"m".repeat(101), 1, 0)],
        (0..17).map(|i| tokens(&format!("m{i}"), 1, 0)).collect(),
    ] {
        let mut l = raised();
        l.agent("w1", done_with("r1", usage, 0));
        assert_eq!(rejected(&fold(&l.0)), [Reject::BadUsage]);
    }
}

#[test]
fn only_a_person_sets_a_price_and_never_backdated() {
    let mut l = raised();
    l.agent("w1", price("m", PriceUnit::Input, 1));
    l.tracker(price("m", PriceUnit::Input, 1));
    l.person(WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Input,
        price_micros: 1,
        effective_from_ms: Some(0),
        as_of: None,
    });
    let s = fold(&l.0);
    assert_eq!(
        rejected(&s),
        [
            Reject::ActorNotAllowed,
            Reject::ActorNotAllowed,
            Reject::BadPrice
        ]
    );
}

/// Below format 8 neither usage nor a price lands, and a reported cost folds as it always did.
#[test]
fn usage_and_prices_wait_for_the_raise() {
    let mut l = held_root();
    l.person(price("m", PriceUnit::Input, 1));
    l.agent("w1", done_with("r1", vec![tokens("m", 1, 0)], 0));
    l.agent("w1", done("r1", 1, 700));
    let s = fold(&l.0);
    assert_eq!(rejected(&s), [Reject::Format, Reject::Format]);
    assert_eq!(s.items[&id("r1")].cost_micros, 700);
}

/// Costs reported before the raise stand, and usage after it adds to them.
#[test]
fn reported_costs_before_the_raise_stand_beside_priced_usage() {
    let mut l = held_root();
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            why: None,
            cost_micros: 700,
            usage: Vec::new(),
            floor_micros: 0,
        },
    );
    l.person(WorkRecord::FormatRaised { to: 8 });
    l.person(price("m", PriceUnit::Input, 1_000_000));
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", done_with("r1", vec![tokens("m", 1_000, 0)], 0));
    let s = fold(&l.0);
    assert_eq!(rejected(&s), []);
    assert_eq!(s.items[&id("r1")].cost_micros, 1_700);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 1_000_000 - 1_700);
    assert_eq!(s.spend().by_model["m"].cost_micros, 1_000);
}

/// A price set for later prices nothing before its start, waiting usage included.
#[test]
fn a_first_price_set_for_later_leaves_earlier_usage_unpriced() {
    let mut l = raised();
    l.agent("w1", done_with("r1", vec![tokens("m", 100_000, 0)], 0));
    l.person(WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Input,
        price_micros: 9_000_000,
        effective_from_ms: Some(1_000_000),
        as_of: None,
    });
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 0);
    assert!(s.spend().by_model["m"].unpriced);
}

#[test]
fn the_models_waiting_for_a_price_are_bounded() {
    let mut l = raised();
    for r in ["r2", "r3"] {
        l.tracker(imported(r));
        l.person(WorkRecord::Accepted {
            item: id(r),
            tier: None,
        });
        l.agent("w1", claimed(r, 1));
    }
    let models = |from: usize, n: usize| -> Vec<Usage> {
        (from..from + n)
            .map(|i| tokens(&format!("m{i}"), 1, 0))
            .collect()
    };
    l.agent("w1", done_with("r1", models(0, 16), 0));
    l.agent("w1", done_with("r2", models(16, 16), 0));
    // 32 is the most a log holds; a 33rd is refused, a repeat is not.
    l.agent("w1", done_with("r3", models(31, 2), 0));
    l.agent("w1", done_with("r3", models(31, 1), 0));
    let s = fold(&l.0);
    assert_eq!(rejected(&s), [Reject::BadUsage]);
    assert_eq!(s.unpriced_models().len(), 32);
}

#[test]
fn an_entry_that_used_nothing_is_left_out() {
    let mut l = raised();
    l.agent("w1", done_with("r1", vec![tokens("m", 0, 0)], 0));
    let s = fold(&l.0);
    assert!(s.spend().by_model.is_empty());
    assert!(s.unpriced_models().is_empty());
}

#[test]
fn prices_in_effect_are_the_latest_that_started() {
    let mut l = raised();
    l.person(price("m", PriceUnit::Input, 1_000_000));
    l.person(WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Input,
        price_micros: 9_000_000,
        effective_from_ms: Some(1_000_000),
        as_of: None,
    });
    l.person(price("m", PriceUnit::Output, 5_000_000));
    let s = fold(&l.0);
    let at = |ms| {
        s.prices_in_effect(ms)
            .into_iter()
            .map(|p| (p.unit, p.price_micros))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        at(u64::MAX),
        [
            (PriceUnit::Input, 9_000_000),
            (PriceUnit::Output, 5_000_000)
        ]
    );
    assert_eq!(
        at(999_999),
        [
            (PriceUnit::Input, 1_000_000),
            (PriceUnit::Output, 5_000_000)
        ]
    );
    assert_eq!(at(0), []);
}

#[test]
fn spend_lists_every_price_set_with_its_start_and_as_of() {
    let mut l = raised9();
    l.person(price("m", PriceUnit::Input, 1_000_000));
    l.person(WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Input,
        price_micros: 2_000_000,
        effective_from_ms: Some(u64::MAX / 2),
        as_of: Some("2026-11-15".into()),
    });
    let spend = fold(&l.0).spend();
    let rows: Vec<_> = spend
        .prices
        .iter()
        .map(|p| {
            (
                p.price_micros,
                p.effective_from_ms == u64::MAX / 2,
                p.as_of.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (1_000_000, false, None),
            (2_000_000, true, Some("2026-11-15"))
        ]
    );
    assert!(fold(&raised().0).spend().prices.is_empty());
}

#[test]
fn a_malformed_as_of_is_refused() {
    let mut l = raised9();
    l.person(WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Input,
        price_micros: 1,
        effective_from_ms: None,
        as_of: Some("last tuesday".into()),
    });
    let s = fold(&l.0);
    assert_eq!(rejected(&s), [Reject::BadPrice]);
    assert!(s.spend().prices.is_empty());
}

#[test]
fn an_impossible_as_of_day_is_refused() {
    for day in [
        "2026-99-99",
        "2026-00-10",
        "2026-02-29",
        "2026-04-31",
        "2026-01-00",
    ] {
        let mut l = raised9();
        l.person(WorkRecord::PriceSet {
            model: "m".into(),
            unit: PriceUnit::Input,
            price_micros: 1,
            effective_from_ms: None,
            as_of: Some(day.into()),
        });
        assert_eq!(rejected(&fold(&l.0)), [Reject::BadPrice], "{day}");
    }
    for day in ["2024-02-29", "2026-12-31", "2000-02-29"] {
        let mut l = raised9();
        l.person(WorkRecord::PriceSet {
            model: "m".into(),
            unit: PriceUnit::Input,
            price_micros: 1,
            effective_from_ms: None,
            as_of: Some(day.into()),
        });
        assert!(rejected(&fold(&l.0)).is_empty(), "{day}");
    }
}

#[test]
fn two_prices_at_one_start_keep_the_later_note() {
    let at = u64::MAX / 2;
    let set = |micros, as_of: Option<&str>| WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Input,
        price_micros: micros,
        effective_from_ms: Some(at),
        as_of: as_of.map(Into::into),
    };
    let mut l = raised9();
    l.person(set(1, Some("2026-11-01")));
    l.person(set(2, None));
    let rows = fold(&l.0).spend().prices;
    assert!(rows.iter().all(|p| p.as_of.is_none()));
}

#[test]
fn unpriced_names_the_unit_that_waits() {
    let mut l = raised();
    l.person(price("m", PriceUnit::Input, 1_000_000));
    l.agent(
        "w1",
        done_with(
            "r1",
            vec![Usage::Tokens {
                model: "m".into(),
                input: 1_000,
                output: 1_000,
                cache_read: 0,
                cache_write: 0,
            }],
            0,
        ),
    );
    let waiting: Vec<_> = fold(&l.0)
        .spend()
        .unpriced
        .into_iter()
        .map(|u| (u.model, u.unit))
        .collect();
    assert_eq!(waiting, [("m".to_owned(), PriceUnit::Output)]);
}
