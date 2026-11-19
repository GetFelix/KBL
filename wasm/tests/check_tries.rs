//! A root's failed checks reach the page with each try's log excerpt, which
//! the state keeps only as an offset.

use kbl_wasm::Folder;
use serde_json::{Value, json};

const URL: &str = "https://github.com/GetFelix/felix/actions/runs/7/job/71";

fn failed(head: &str, log: &str, fix: bool) -> Value {
    json!({ "v": 23, "by": "ingest", "actor": "tracker", "kind": "check_failed", "item": "r1",
            "pr": "GetFelix/felix#9", "check": "lint", "url": URL, "head": head, "log": log, "fix": fix })
}

#[test]
fn every_try_shows_its_check_head_and_log() {
    let mut fold = Folder::new();
    let log = [
        json!({ "v": 23, "by": "gabriel", "actor": "person", "kind": "format_raised", "to": 23 }),
        json!({ "by": "ingest", "actor": "tracker", "kind": "imported", "item": "r1",
                "source": "github", "upstream_id": "GetFelix/felix#1", "title": "fix the lint" }),
        json!({ "by": "gabriel", "actor": "person", "kind": "accepted", "item": "r1" }),
        json!({ "by": "w1", "actor": "agent", "kind": "claimed", "item": "r1", "attempt": 1,
                "deadline_ms": 1_800_000_090_000u64 }),
        json!({ "by": "w1", "actor": "agent", "kind": "progress", "item": "r1", "attempt": 1,
                "note": { "text": "opened" }, "pr": "GetFelix/felix#9" }),
        json!({ "by": "w1", "actor": "agent", "kind": "done", "item": "r1", "attempt": 1, "cost_micros": 0 }),
        json!({ "by": "ingest", "actor": "tracker", "kind": "pr_status", "item": "r1",
                "status": { "pr": "GetFelix/felix#9", "phase": "open", "review": "unreviewed",
                            "checks": "red", "failing": { "name": "lint", "url": URL } } }),
        failed("abc", "error: unused import `Foo`", true),
        // The same try again is refused and keeps nothing.
        failed("abc", "a different excerpt", false),
        failed("def", "error: still unused", false),
    ];
    for (i, r) in log.iter().enumerate() {
        let mut rec = r.clone();
        rec["at_ms"] = json!(1000 + i);
        fold.apply(i as f64, rec.to_string().as_bytes());
    }
    let items: Vec<Value> = serde_json::from_str(&fold.items_json()).unwrap();
    let root = items.iter().find(|i| i["id"] == "r1").unwrap();
    let tries = root["check_tries"].as_array().unwrap();
    assert_eq!(tries.len(), 2, "{tries:?}");
    assert_eq!(tries[0]["check"], "lint");
    assert_eq!(tries[0]["head"], "abc");
    assert_eq!(tries[0]["log"], "error: unused import `Foo`");
    assert_eq!(tries[0]["url"], URL);
    assert!(tries[0]["fix"].is_string(), "the first try opened a fix");
    assert_eq!(tries[1]["head"], "def");
    assert_eq!(tries[1]["log"], "error: still unused");
    assert!(tries[1]["fix"].is_null());
    let child = items.iter().find(|i| i["work"] == "fix_check").unwrap();
    assert_eq!(child["check_tries"], json!([]));
}
