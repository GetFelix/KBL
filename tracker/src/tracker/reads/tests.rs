use kbl_core::names::ItemId;
use kbl_core::work::WorkRecord;

use super::about;

fn id(s: &str) -> ItemId {
    ItemId::new(s).unwrap()
}

/// A prerequisite names its waiter in `item`; the history of the item it
/// creates or points at, and of the triage item that posted it, shows it too.
#[test]
fn a_record_that_points_at_an_item_is_in_its_history() {
    let prereq: WorkRecord = serde_json::from_value(serde_json::json!({
        "kind": "prerequisite", "item": "r1", "attempt": 1, "prerequisite": "g1",
        "key": "need", "triage": "t1"
    }))
    .unwrap();
    for (item, is) in [("r1", true), ("g1", true), ("t1", true), ("x", false)] {
        assert_eq!(about(&prereq, &id(item)), is, "{item}");
    }
    let dup: WorkRecord = serde_json::from_value(serde_json::json!({
        "kind": "triaged", "item": "r2", "tier": 1, "why": "same bug", "duplicate_of": "r1"
    }))
    .unwrap();
    assert!(about(&dup, &id("r1")));
    assert!(!about(&dup, &id("x")));
}

use crate::tracker::Tracker;
use crate::tracker::tests::{Fake, item, key, seed, start};

/// `a1` and ten other items, with a1's tier changed twice after each: 22
/// records about a1 among 32.
async fn mixed() -> (Fake, Tracker<Fake>) {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    for i in 0..10 {
        let other = format!("x{i}");
        let imported = WorkRecord::Imported {
            item: item(&other),
            source: "gh".into(),
            upstream_id: other.clone(),
            title: other.clone(),
        };
        t.person_record(key("gab"), imported).await.unwrap();
        for tier in [2, 1] {
            let tier = WorkRecord::TierChanged {
                item: item("a1"),
                tier,
            };
            t.person_record(key("gab"), tier).await.unwrap();
        }
    }
    (fake, t)
}

fn offsets(v: &serde_json::Value) -> Vec<u64> {
    v["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["offset"].as_u64().unwrap())
        .collect()
}

/// Follows `next` from 0 and returns each answer's offsets.
async fn walk(t: &Tracker<Fake>, most: usize, scan: usize) -> Vec<Vec<u64>> {
    let mut answers = Vec::new();
    let mut from = 0;
    loop {
        let v = t
            .history_capped(&item("a1"), from, most, scan)
            .await
            .unwrap();
        answers.push(offsets(&v));
        match v["next"].as_u64() {
            Some(next) => from = next,
            None => return answers,
        }
        assert!(answers.len() < 50, "next never ran out");
    }
}

#[tokio::test]
async fn history_pages_by_matches_without_skips_or_repeats() {
    let (fake, t) = mixed().await;
    let all = offsets(&t.history(&item("a1"), 0).await.unwrap());
    assert_eq!(all.len(), 22);
    // Felix answers three records a read, so the tracker reads several per answer.
    fake.page_size(3);
    // Three matches an answer: each stops on a record about a1.
    let answers = walk(&t, 3, 1000).await;
    let sizes: Vec<usize> = answers.iter().map(Vec::len).collect();
    assert_eq!(sizes, [3, 3, 3, 3, 3, 3, 3, 1]);
    assert_eq!(answers.concat(), all);
}

#[tokio::test]
async fn history_stops_at_the_scan_cap_mid_read_and_goes_on_from_there() {
    let (fake, t) = mixed().await;
    let all = offsets(&t.history(&item("a1"), 0).await.unwrap());
    fake.page_size(3);
    // Five scanned per answer ends inside a three-record read.
    let answers = walk(&t, 100, 5).await;
    assert_eq!(answers.len(), 7, "{answers:?}");
    assert_eq!(answers.concat(), all);
}

#[tokio::test]
async fn history_from_past_the_end_is_empty_and_keeps_the_connection() {
    let (fake, t) = mixed().await;
    fake.page_size(3);
    let v = t.history(&item("a1"), 1_000_000).await.unwrap();
    assert_eq!(v["records"], serde_json::json!([]));
    assert_eq!(v["next"], serde_json::Value::Null);
    assert!(fake.forgets().is_empty());
}
