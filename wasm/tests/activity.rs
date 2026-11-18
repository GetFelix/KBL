//! The page's timeline and history read what happened on an item from the records the
//! fold already applied: nothing is added to the log.

use kbl_wasm::Folder;
use serde_json::{Value, json};

fn feed(fold: &mut Folder, who: (&str, &str), records: &[Value]) {
    for r in records {
        let mut rec = json!({ "at_ms": 1000, "by": who.0, "actor": who.1 });
        rec.as_object_mut()
            .unwrap()
            .extend(r.as_object().unwrap().clone());
        let at = fold.at() + 1.0;
        assert!(fold.apply(at, rec.to_string().as_bytes()), "{r}");
    }
}

#[test]
fn an_item_keeps_its_steps_attempts_and_what_each_cost() {
    let mut fold = Folder::new();
    feed(
        &mut fold,
        ("ingest", "tracker"),
        &[
            json!({ "kind": "imported", "item": "r1", "source": "github",
                  "upstream_id": "GetFelix/felix#1", "title": "r1" }),
        ],
    );
    feed(
        &mut fold,
        ("gabriel", "person"),
        &[json!({ "kind": "accepted", "item": "r1", "tier": 1 })],
    );
    let long = "x".repeat(2000) + "tail";
    feed(
        &mut fold,
        ("w1", "agent"),
        &[
            json!({ "kind": "claimed", "item": "r1", "attempt": 1, "deadline_ms": 9_000_000_000_000u64 }),
            json!({ "kind": "step", "item": "r1", "attempt": 1, "command": "cargo test",
                    "exit_code": 101, "output": { "text": long } }),
            json!({ "kind": "done", "item": "r1", "attempt": 1, "cost_micros": 250_000 }),
        ],
    );
    let ev: Vec<Value> = serde_json::from_str(&fold.activity_json("r1")).unwrap();
    let kinds: Vec<&str> = ev.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["accepted", "claimed", "step", "done"]);
    let step = &ev[2];
    assert_eq!(step["command"], "cargo test");
    assert_eq!(step["exit_code"], 101);
    let out = step["output"].as_str().unwrap();
    assert!(
        out.len() <= 800 && out.ends_with("tail"),
        "only the tail of the output is kept"
    );
    assert_eq!(ev[3]["cost_micros"], 250_000);
    assert_eq!(ev[3]["attempt"], 1);

    let items: Value = serde_json::from_str(&fold.items_json()).unwrap();
    assert_eq!(
        items[0]["hold_deadline_ms"],
        Value::Null,
        "a finished attempt holds nothing"
    );
    assert_eq!(fold.activity_json("nope"), "[]");
    assert_eq!(fold.activity_json("not a name!"), "[]");
}

#[test]
fn a_held_item_reports_when_the_hold_lapses() {
    let mut fold = Folder::new();
    feed(
        &mut fold,
        ("ingest", "tracker"),
        &[
            json!({ "kind": "imported", "item": "r1", "source": "github",
        "upstream_id": "GetFelix/felix#1", "title": "r1" }),
        ],
    );
    feed(
        &mut fold,
        ("gabriel", "person"),
        &[json!({ "kind": "accepted", "item": "r1", "tier": 1 })],
    );
    feed(
        &mut fold,
        ("w1", "agent"),
        &[json!({ "kind": "claimed", "item": "r1", "attempt": 1, "deadline_ms": 777 })],
    );
    let items: Value = serde_json::from_str(&fold.items_json()).unwrap();
    assert_eq!(items[0]["hold_deadline_ms"], 777);
}

/// The panel must never show a refused record as having happened: the fold marks it, with
/// the reason it gave, from the same verdict that left the state alone.
#[test]
fn a_refused_record_is_marked_refused_and_the_item_stays_held() {
    let mut fold = Folder::new();
    feed(
        &mut fold,
        ("ingest", "tracker"),
        &[
            json!({ "kind": "imported", "item": "r1", "source": "github",
                    "upstream_id": "GetFelix/felix#1", "title": "r1" }),
        ],
    );
    feed(
        &mut fold,
        ("gabriel", "person"),
        &[
            json!({ "kind": "format_raised", "to": 8, "v": 8 }),
            json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
        ],
    );
    feed(
        &mut fold,
        ("w1", "agent"),
        &[
            json!({ "kind": "claimed", "item": "r1", "attempt": 1, "deadline_ms": 9_000_000_000_000u64 }),
        ],
    );
    // From format 8 a report may not carry a cost of its own.
    let refused = json!({ "at_ms": 1000, "by": "w1", "actor": "agent", "kind": "done",
                          "item": "r1", "attempt": 1, "cost_micros": 5 });
    let at = fold.at() + 1.0;
    assert!(fold.apply(at, refused.to_string().as_bytes()));

    let items: Value = serde_json::from_str(&fold.items_json()).unwrap();
    assert_eq!(
        items[0]["holder"], "w1",
        "the refused done released nothing"
    );
    let ev: Vec<Value> = serde_json::from_str(&fold.activity_json("r1")).unwrap();
    let done = ev.iter().find(|e| e["kind"] == "done").unwrap();
    assert_eq!(done["refused"], "bad_usage");
    assert!(
        ev.iter()
            .filter(|e| e["kind"] != "done")
            .all(|e| e.get("refused").is_none()),
        "an accepted record is not marked"
    );
}
