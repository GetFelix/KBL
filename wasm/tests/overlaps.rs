//! The page reads overlaps from the `files` entries it folded, keeping only
//! each item's current one.

use kbl_wasm::Folder;
use serde_json::{Value, json};

fn feed(fold: &mut Folder, records: &[Value]) {
    for (i, r) in records.iter().enumerate() {
        let mut rec = json!({ "at_ms": i, "by": "gabriel", "actor": "person" });
        rec.as_object_mut()
            .unwrap()
            .extend(r.as_object().unwrap().clone());
        if r["kind"] == "imported" {
            rec["by"] = json!("ingest");
            rec["actor"] = json!("tracker");
        }
        let at = fold.at() + 1.0;
        assert!(fold.apply(at, rec.to_string().as_bytes()), "record {i}");
    }
}

fn root(item: &str) -> [Value; 2] {
    [
        json!({ "kind": "imported", "item": item, "source": "github",
                "upstream_id": format!("GetFelix/felix#{item}"), "title": item }),
        json!({ "kind": "accepted", "item": item, "tier": 1 }),
    ]
}

fn files(item: &str, text: &str) -> Value {
    json!({ "kind": "context", "item": item, "entry": "files", "text": { "text": text } })
}

#[test]
fn overlaps_follow_the_current_files_entries() {
    let mut fold = Folder::new();
    let mut log: Vec<Value> = root("r1").into_iter().chain(root("r2")).collect();
    log.push(files("r1", "core/src/work.rs\n"));
    log.push(files("r2", "core/src/\n"));
    feed(&mut fold, &log);
    let found: Value = serde_json::from_str(&fold.overlaps_json()).unwrap();
    assert_eq!(
        found,
        json!([{ "first": "r1", "then": "r2", "files": ["core/src/work.rs"] }])
    );

    feed(&mut fold, &[files("r2", "docs/\n")]);
    assert_eq!(fold.overlaps_json(), "[]");
}
