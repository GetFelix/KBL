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
