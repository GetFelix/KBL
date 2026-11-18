use serde_json::json;

use super::*;

const H: u64 = 3_600_000;

fn rec(at: u64, by: (&str, &str), body: Value) -> Vec<u8> {
    let mut r = json!({ "at_ms": at, "by": by.0, "actor": by.1, "v": 8 });
    r.as_object_mut()
        .unwrap()
        .extend(body.as_object().unwrap().clone());
    r.to_string().into_bytes()
}

/// One item a person accepted and closed as completed.
fn log() -> Vec<(u64, Vec<u8>)> {
    let t = ("ingest", "tracker");
    let p = ("gabriel", "person");
    let day = 24 * H;
    vec![
        rec(0, p, json!({ "kind": "format_raised", "to": 8, "v": 8 })),
        rec(day, t, json!({ "kind": "imported", "item": "i1", "source": "github", "upstream_id": "o/r#1", "title": "i1" })),
        rec(day + H, p, json!({ "kind": "accepted", "item": "i1", "tier": 1 })),
        rec(day + 2 * H, p, json!({ "kind": "closed", "item": "i1", "why": "completed" })),
    ]
    .into_iter()
    .enumerate()
    .map(|(n, r)| (n as u64, r))
    .collect()
}

#[test]
fn exports_the_figures_the_fold_reports_and_the_hand_logged_minutes() {
    let out = export(&log(), "arm-a", 7, 3 * 24 * H, Some(30.0)).unwrap();
    assert_eq!(out["label"], "arm-a");
    assert_eq!(out["records"], 4);
    assert_eq!(out["summary"]["done"], out["report"]["throughput"]["done"]);
    assert_eq!(out["summary"]["done"], 1, "{out}");
    assert_eq!(out["summary"]["touches_per_done"], 2.0);
    assert_eq!(out["summary"]["engineer_minutes_per_done"], 30.0);
}

#[test]
fn an_empty_window_has_no_per_item_figures() {
    let out = export(&[], "empty", 7, 24 * H, Some(10.0)).unwrap();
    assert_eq!(out["summary"]["done"], 0);
    assert!(out["summary"]["touches_per_done"].is_null());
    assert!(out["summary"]["engineer_minutes_per_done"].is_null());
    let text = csv(&out);
    assert!(text.starts_with("name,value\nlabel,empty\n"), "{text}");
    assert!(text.contains("touches_per_done,\n"), "{text}");
}

#[test]
fn a_window_outside_one_to_ninety_days_is_refused() {
    assert!(export(&[], "x", 0, 0, None).is_err());
    assert!(export(&[], "x", 91, 0, None).is_err());
}

#[test]
fn a_record_newer_than_this_build_stops_the_export() {
    let newer = rec(
        1,
        ("gabriel", "person"),
        json!({ "kind": "format_raised", "to": 9999, "v": 9999 }),
    );
    assert!(export(&[(0, newer)], "x", 7, 0, None).is_err());
}
