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
    assert_eq!(spend.unpriced.iter().collect::<Vec<_>>(), ["new"]);

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
