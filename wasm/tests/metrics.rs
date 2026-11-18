//! The Metrics dialog's numbers come from accepted records only: a refused record, a logged
//! refusal and a redo each count once or not at all.

use kbl_wasm::Folder;
use serde_json::{Value, json};

const H: u64 = 3_600_000;
const DAY: u64 = 24 * H;

fn put(fold: &mut Folder, t: u64, who: (&str, &str), r: Value) -> bool {
    let mut rec = json!({ "at_ms": t, "by": who.0, "actor": who.1, "v": 8 });
    rec.as_object_mut()
        .unwrap()
        .extend(r.as_object().unwrap().clone());
    let at = fold.at() + 1.0;
    assert!(fold.apply(at, rec.to_string().as_bytes()), "{r}");
    true
}

const T: (&str, &str) = ("ingest", "tracker");
const P: (&str, &str) = ("gabriel", "person");
const A: (&str, &str) = ("w1", "agent");

fn import(fold: &mut Folder, t: u64, id: &str, n: u32) {
    put(
        fold,
        t,
        T,
        json!({ "kind": "imported", "item": id, "source": "github",
                "upstream_id": format!("GetFelix/felix#{n}"), "title": id }),
    );
}

fn setup() -> Folder {
    let mut f = Folder::new();
    put(
        &mut f,
        0,
        P,
        json!({ "kind": "format_raised", "to": 8, "v": 8 }),
    );
    for (unit, price) in [("input", 1_000_000u64), ("output", 2_000_000)] {
        put(
            &mut f,
            0,
            P,
            json!({ "kind": "price_set", "model": "m1", "unit": unit, "price_micros": price }),
        );
    }
    f
}

fn report(f: &Folder, now: u64, days: u32) -> Value {
    serde_json::from_str(&f.metrics_json(now as f64, days)).unwrap()
}

fn stage<'a>(r: &'a Value, name: &str) -> &'a Value {
    r["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["stage"] == name)
        .unwrap()
}

/// r1 waits an hour in triage, is accepted, held for 2 h, and sits in review for 4 h.
fn merged_fix(f: &mut Folder, id: &str, n: u32, t0: u64) {
    import(f, t0, id, n);
    put(
        f,
        t0 + H,
        P,
        json!({ "kind": "accepted", "item": id, "tier": 1 }),
    );
    put(
        f,
        t0 + H,
        A,
        json!({ "kind": "claimed", "item": id, "attempt": 1, "deadline_ms": 9_000_000_000_000u64 }),
    );
    put(
        f,
        t0 + 3 * H,
        A,
        json!({ "kind": "done", "item": id, "attempt": 1,
                "usage": [{ "unit": "tokens", "model": "m1", "input": 100, "output": 50 }] }),
    );
    put(
        f,
        t0 + 7 * H,
        P,
        json!({ "kind": "closed", "item": id, "why": "merged" }),
    );
}

#[test]
fn stage_times_throughput_cost_and_supervision_come_from_the_records() {
    let mut f = setup();
    merged_fix(&mut f, "r1", 1, 1000);
    merged_fix(&mut f, "r2", 2, 1000 + 10 * H);
    let r = report(&f, 2 * DAY, 7);

    // Triage 1 h, held 2 h, in review 4 h, for each of the two.
    assert_eq!(stage(&r, "triage")["n"], 2);
    assert_eq!(stage(&r, "triage")["p50_ms"], H);
    assert_eq!(stage(&r, "held")["p85_ms"], 2 * H);
    assert_eq!(stage(&r, "in_review")["p50_ms"], 4 * H);
    assert_eq!(
        stage(&r, "blocked")["n"],
        0,
        "an empty stage has no percentile"
    );
    assert_eq!(stage(&r, "blocked")["p50_ms"], Value::Null);

    let t = &r["throughput"];
    assert_eq!(t["done"], 2);
    let per_day: Vec<u64> = serde_json::from_value(t["per_day"].clone()).unwrap();
    assert_eq!(per_day.len(), 7);
    assert_eq!(per_day.iter().sum::<u64>(), 2);
    assert_eq!(
        per_day[5], 2,
        "both merged on day 0 of the log, the second to last day"
    );

    // 100 input at 1 micro + 50 output at 2 micros = 200 micros each.
    let c = &r["cost"];
    assert_eq!(c["total_micros"], 400);
    assert_eq!(c["per_done_micros"], 200);
    assert_eq!(c["by_model"][0]["model"], "m1");
    assert_eq!(c["by_model"][0]["per_done_micros"], 200);
    assert_eq!(c["unattributed_micros"], 0);

    // A person wrote accepted and closed on each: 2 touches per done item, all touched.
    let s = &r["supervision"];
    assert_eq!(s["touched"], 2);
    assert_eq!(s["touches_per_done"], 2.0);
    assert_eq!(s["touched_share_permille"], 1000);
    assert_eq!(
        s["wait_on_person"]["p50_ms"],
        4 * H,
        "review is time waiting on a person"
    );
}

#[test]
fn an_item_done_outside_the_window_is_left_out() {
    let mut f = setup();
    merged_fix(&mut f, "r1", 1, 1000);
    let r = report(&f, 30 * DAY, 7);
    assert_eq!(r["throughput"]["done"], 0);
    assert_eq!(stage(&r, "triage")["n"], 0);
    assert_eq!(
        r["cost"]["per_done_micros"],
        Value::Null,
        "nothing done has no average, not zero"
    );
    assert_eq!(r["rework"]["redos_per_done"], Value::Null);
    assert_eq!(report(&f, 30 * DAY, 90)["throughput"]["done"], 1);
}

#[test]
fn refused_records_count_for_nothing() {
    let mut f = setup();
    import(&mut f, 1000, "r1", 1);
    put(
        &mut f,
        1000 + H,
        P,
        json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
    );
    // Reopening an open item is refused by the fold, and the tracker logs a refusal besides.
    put(
        &mut f,
        1000 + 2 * H,
        P,
        json!({ "kind": "reopened", "item": "r1" }),
    );
    put(
        &mut f,
        1000 + 2 * H,
        T,
        json!({ "kind": "refused", "item": "r1", "op": "claim", "reason": "limit" }),
    );
    // An agent that does not hold the item cannot release it, so no failed attempt.
    put(
        &mut f,
        1000 + 2 * H,
        A,
        json!({ "kind": "released", "item": "r1", "attempt": 1 }),
    );
    put(
        &mut f,
        1000 + 3 * H,
        P,
        json!({ "kind": "closed", "item": "r1", "why": "completed" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(r["throughput"]["done"], 1);
    assert_eq!(r["rework"]["reopened"], 0);
    assert_eq!(r["rework"]["failed_attempts"], 0);
    // accepted and closed only.
    assert_eq!(r["supervision"]["touches"], 2);
}

#[test]
fn a_redo_or_reopen_counts_the_item_once() {
    let mut f = setup();
    merged_fix(&mut f, "r1", 1, 1000);
    put(
        &mut f,
        8 * H,
        P,
        json!({ "kind": "reopened", "item": "r1" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(r["throughput"]["done"], 0, "reopened, so not done now");
    put(
        &mut f,
        9 * H,
        P,
        json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
    );
    put(
        &mut f,
        11 * H,
        P,
        json!({ "kind": "closed", "item": "r1", "why": "merged" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(r["throughput"]["done"], 1);
    assert_eq!(r["rework"]["reopened"], 1);
    assert_eq!(r["rework"]["reopened_per_done"], 1.0);
    assert_eq!(
        r["cost"]["per_done_micros"], 200,
        "the one attempt is charged once"
    );
    let per_day: Vec<u64> = serde_json::from_value(r["throughput"]["per_day"].clone()).unwrap();
    assert_eq!(per_day.iter().sum::<u64>(), 1);
}

#[test]
fn failed_attempts_are_rework_and_waits_are_not() {
    let mut f = setup();
    import(&mut f, 1000, "r1", 1);
    put(
        &mut f,
        1000,
        P,
        json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
    );
    for (n, why) in [(1u32, json!("failed")), (2, json!("waits_on_outside"))] {
        put(
            &mut f,
            1000 + u64::from(n) * H,
            A,
            json!({ "kind": "claimed", "item": "r1", "attempt": n, "deadline_ms": 9_000_000_000_000u64 }),
        );
        put(
            &mut f,
            1000 + u64::from(n) * H,
            A,
            json!({ "kind": "released", "item": "r1", "attempt": n, "why": why }),
        );
    }
    put(
        &mut f,
        1000 + 5 * H,
        P,
        json!({ "kind": "closed", "item": "r1", "why": "completed" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(r["rework"]["failed_attempts"], 1);
}

#[test]
fn a_clock_that_steps_back_gives_no_negative_or_huge_stay() {
    let mut f = setup();
    import(&mut f, 10 * H, "r1", 1);
    put(
        &mut f,
        5 * H,
        P,
        json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
    );
    put(
        &mut f,
        6 * H,
        P,
        json!({ "kind": "closed", "item": "r1", "why": "completed" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(stage(&r, "triage")["p50_ms"], 0);
    assert_eq!(r["throughput"]["done"], 1);
}

#[test]
fn closing_an_item_ends_the_blocked_stay_of_one_in_another_tree() {
    let mut f = setup();
    import(&mut f, 0, "a", 1);
    import(&mut f, 0, "b", 2);
    for id in ["a", "b"] {
        put(
            &mut f,
            0,
            P,
            json!({ "kind": "accepted", "item": id, "tier": 1 }),
        );
    }
    put(
        &mut f,
        0,
        P,
        json!({ "kind": "linked", "item": "b", "to": "a" }),
    );
    put(
        &mut f,
        5 * H,
        P,
        json!({ "kind": "closed", "item": "a", "why": "completed" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(stage(&r, "blocked")["n"], 1, "b left Blocked when a closed");
    assert_eq!(stage(&r, "blocked")["p50_ms"], 5 * H);
}

#[test]
fn a_budget_refusal_ends_the_stay_it_sends_the_root_out_of() {
    let mut f = setup();
    import(&mut f, 0, "r1", 1);
    put(
        &mut f,
        H,
        P,
        json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
    );
    put(
        &mut f,
        H,
        A,
        json!({ "kind": "claimed", "item": "r1", "attempt": 1, "deadline_ms": 9_000_000_000_000u64 }),
    );
    // The refusal is no work, but it puts the root in Needs you.
    put(
        &mut f,
        3 * H,
        T,
        json!({ "kind": "refused", "item": "r1", "op": "claim", "reason": "budget" }),
    );
    put(
        &mut f,
        5 * H,
        P,
        json!({ "kind": "closed", "item": "r1", "why": "completed" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(stage(&r, "held")["n"], 1);
    assert_eq!(stage(&r, "held")["p50_ms"], 2 * H);
    assert_eq!(stage(&r, "needs_you")["n"], 1);
    assert_eq!(stage(&r, "needs_you")["p50_ms"], 2 * H);
}

#[test]
fn a_progress_record_that_changes_the_request_ends_the_failed_checks_stay() {
    let mut f = setup();
    import(&mut f, 0, "r1", 1);
    put(
        &mut f,
        H,
        P,
        json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
    );
    put(
        &mut f,
        H,
        A,
        json!({ "kind": "claimed", "item": "r1", "attempt": 1, "deadline_ms": 9_000_000_000_000u64 }),
    );
    put(
        &mut f,
        H,
        A,
        json!({ "kind": "progress", "item": "r1", "attempt": 1, "note": { "text": "pushed" }, "pr": "GetFelix/felix#1" }),
    );
    put(
        &mut f,
        2 * H,
        A,
        json!({ "kind": "done", "item": "r1", "attempt": 1 }),
    );
    put(
        &mut f,
        3 * H,
        T,
        json!({ "kind": "pr_status", "item": "r1", "status": { "pr": "GetFelix/felix#1",
                "phase": "open", "review": "unreviewed", "checks": "red",
                "failing": { "name": "ci", "url": "https://x/1" } } }),
    );
    // A person points the item at another request, which starts without a status.
    put(
        &mut f,
        5 * H,
        P,
        json!({ "kind": "progress", "item": "r1", "attempt": 1, "note": { "text": "new pr" }, "pr": "GetFelix/felix#2" }),
    );
    let r = report(&f, DAY, 7);
    assert_eq!(stage(&r, "needs_you")["n"], 1);
    assert_eq!(stage(&r, "needs_you")["p50_ms"], 2 * H);
}

#[test]
fn a_clock_behind_the_records_keeps_the_latest_stays_and_says_so() {
    let mut f = setup();
    merged_fix(&mut f, "r1", 1, 10 * DAY);
    // The browser thinks it is a day before the item merged.
    let r = report(&f, 10 * DAY - DAY, 7);
    assert_eq!(r["clock_behind"], true);
    assert_eq!(r["to_ms"], 7 * H + 10 * DAY);
    assert_eq!(r["throughput"]["done"], 1);
    assert_eq!(stage(&r, "in_review")["n"], 1);
    let ok = report(&f, 11 * DAY, 7);
    assert_eq!(ok["clock_behind"], false);
}

#[test]
fn a_refusal_for_an_unknown_item_does_not_move_the_window() {
    let mut f = setup();
    merged_fix(&mut f, "r1", 1, 10 * DAY);
    put(
        &mut f,
        400 * DAY,
        T,
        json!({ "kind": "refused", "item": "nope", "op": "claim", "reason": "limit" }),
    );
    let r = report(&f, 11 * DAY, 7);
    assert_eq!(r["clock_behind"], false);
    assert_eq!(r["throughput"]["done"], 1);
}
