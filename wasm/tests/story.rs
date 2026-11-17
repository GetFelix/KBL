//! The page reads an item's story from the records it folded: decisions by
//! phase with their edits and evidence, and the flags over them.

use kbl_wasm::Folder;
use serde_json::{Value, json};

const LOG: &str = include_str!("../golden/story.jsonl");

fn fold() -> Folder {
    let mut fold = Folder::new();
    for (i, line) in LOG.lines().enumerate() {
        assert!(fold.apply(i as f64, line.as_bytes()), "record {i}");
    }
    fold
}

#[test]
fn a_story_is_built_from_the_fixture_log() {
    let stories: Value = serde_json::from_str(&fold().stories_json()).unwrap();
    assert_eq!(
        stories.as_object().unwrap().len(),
        1,
        "only r1 has decisions"
    );
    let s = &stories["r1"];

    let phases: Vec<_> = s["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["phase"].clone())
        .collect();
    assert_eq!(
        phases,
        [
            json!("reproduce"),
            json!("cause"),
            json!("fix"),
            json!("verify")
        ]
    );

    let fix = &s["groups"][2]["decisions"][0];
    assert_eq!(fix["offset"], 7);
    assert_eq!(fix["edits"][0]["file"], "core/src/cache.rs");
    assert_eq!(
        fix["evidence"][0]["found"],
        json!({ "kind": "step", "command": "cargo test cache", "exit_code": 101 })
    );

    assert_eq!(s["people"][0]["by"], "gabriel");
    let rules: Vec<_> = s["flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["rule"].clone(), f["decision"].clone()))
        .collect();
    assert_eq!(
        rules,
        [
            (json!("claims_pass_without_run"), json!(9)),
            (json!("edits_never_run"), Value::Null)
        ]
    );
}
