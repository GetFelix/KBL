//! The fold the page runs must give the state the tracker's fold gives, on the
//! logs the core tests already pin. The page's WebAssembly build is checked
//! against the same `.state.json` files by `dashboard/test/fold.test.ts`, so a
//! change to the fold shows up in both until the files are regenerated:
//! `WRITE_GOLDEN=1 cargo test -p kbl-wasm --test golden`.

use kbl_core::codec::Codec;
use kbl_core::work::{WorkLogged, WorkRecord};
use kbl_core::work_fold::fold;
use kbl_wasm::Folder;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden");

/// Who logs a record. Everyone is a person, except the records of the reviewed request
/// (`rv` and its reviews `v1`, `v2`), which only the tracker and agents may write.
fn who(rec: &WorkRecord) -> (&'static str, &'static str) {
    let item = serde_json::to_value(rec).unwrap()["item"]
        .as_str()
        .map(str::to_owned);
    let reviewed = matches!(item.as_deref(), Some("rv" | "v1" | "v2"));
    match (reviewed, rec.kind(), item.as_deref()) {
        (false, ..) | (_, "imported" | "accepted", _) => ("gabriel", "person"),
        (_, "pr_status" | "proposed", _) => ("ingest", "tracker"),
        (_, _, Some("v1")) => ("w2", "agent"),
        (_, _, Some("v2")) => ("w3", "agent"),
        _ => ("w1", "agent"),
    }
}

fn logs() -> Vec<(&'static str, String)> {
    let core = concat!(env!("CARGO_MANIFEST_DIR"), "/../core/tests");
    let every = std::fs::read_to_string(format!("{core}/work_wire.jsonl")).unwrap();
    // Wrap each bare record the way the tracker logs it, so every kind is folded.
    let wrapped: String = every
        .lines()
        .enumerate()
        .map(|(i, l)| {
            let rec = WorkRecord::decode(l.as_bytes()).unwrap();
            let (by, actor) = who(&rec);
            let logged = serde_json::json!({ "at_ms": 1000 + i, "by": by, "actor": actor });
            let mut logged = logged.as_object().unwrap().clone();
            let body = serde_json::to_value(&rec).unwrap();
            logged.extend(body.as_object().unwrap().clone());
            serde_json::to_string(&logged).unwrap() + "\n"
        })
        .collect();
    vec![
        ("every_record", wrapped),
        (
            "pre_prerequisite",
            std::fs::read_to_string(format!("{core}/work_pre_prerequisite.jsonl")).unwrap(),
        ),
        (
            "pre_questions",
            std::fs::read_to_string(format!("{core}/work_pre_questions.jsonl")).unwrap(),
        ),
        (
            "format_2",
            std::fs::read_to_string(format!("{core}/work_format_2.jsonl")).unwrap(),
        ),
        (
            "format_3",
            std::fs::read_to_string(format!("{core}/work_format_3.jsonl")).unwrap(),
        ),
        (
            "agent_context",
            std::fs::read_to_string(format!("{core}/work_agent_context.jsonl")).unwrap(),
        ),
    ]
}

#[test]
fn the_page_fold_matches_the_core_fold_on_the_golden_logs() {
    for (name, log) in logs() {
        let path = format!("{DIR}/{name}.jsonl");
        let state_path = format!("{DIR}/{name}.state.json");
        let log_pairs: Vec<(u64, WorkLogged)> = log
            .lines()
            .enumerate()
            .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
            .collect();
        let native = serde_json::to_string(&fold(&log_pairs)).unwrap();

        let mut folder = Folder::new();
        for (i, l) in log.lines().enumerate() {
            assert!(folder.apply(i as f64, l.as_bytes()), "{name}:{i}");
        }
        assert_eq!(folder.state_json(), native, "{name}");

        if std::env::var_os("WRITE_GOLDEN").is_some() {
            std::fs::write(&path, &log).unwrap();
            std::fs::write(&state_path, &native).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), log, "{name} log");
        assert_eq!(
            std::fs::read_to_string(&state_path).unwrap(),
            native,
            "{name} state"
        );
    }
}

#[test]
fn a_repeat_or_a_bad_payload_changes_nothing() {
    let mut f = Folder::new();
    let rec = br#"{"at_ms":1,"by":"gabriel","actor":"person","kind":"imported","item":"r1","source":"github","upstream_id":"a#1","title":"t"}"#;
    assert!(f.apply(3.0, rec));
    let before = f.state_json();
    assert!(!f.apply(3.0, rec), "same offset");
    assert!(!f.apply(2.0, rec), "older offset");
    assert!(!f.apply(4.0, b"not json"));
    assert_eq!(f.state_json(), before);
    assert_eq!(f.at(), 3.0);
}

#[test]
fn a_record_of_a_newer_format_stops_the_fold() {
    let mut f = Folder::new();
    let rec = br#"{"at_ms":1,"by":"gabriel","actor":"person","kind":"imported","item":"r1","source":"github","upstream_id":"a#1","title":"t"}"#;
    assert!(f.apply(0.0, rec));
    assert_eq!(f.newer(), 0);
    let newer =
        br#"{"v":999,"at_ms":2,"by":"gabriel","actor":"person","kind":"accepted","item":"r1"}"#;
    assert!(!f.apply(1.0, newer));
    assert_eq!(f.newer(), 999);
    // Nothing after it is applied either: the state would skip a record.
    let before = f.state_json();
    let next = br#"{"at_ms":3,"by":"gabriel","actor":"person","kind":"accepted","item":"r1"}"#;
    assert!(!f.apply(2.0, next));
    assert_eq!(f.state_json(), before);
    assert_eq!(f.at(), 0.0);
}
