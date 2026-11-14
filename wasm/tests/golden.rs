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
            "format_4",
            std::fs::read_to_string(format!("{core}/work_format_4.jsonl")).unwrap(),
        ),
        (
            "format_5",
            std::fs::read_to_string(format!("{core}/work_format_5.jsonl")).unwrap(),
        ),
        (
            "format_6",
            std::fs::read_to_string(format!("{core}/work_format_6.jsonl")).unwrap(),
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
        let spend = serde_json::to_string(&fold(&log_pairs).spend()).unwrap();
        assert_eq!(folder.spend_json(), spend, "{name} spend");

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

#[test]
fn blocked_names_each_blocker_with_its_own_repo() {
    let mut f = Folder::new();
    let recs = [
        r#"{"at_ms":1,"by":"ingest","actor":"tracker","kind":"imported","item":"blk","source":"github","upstream_id":"GetFelix/felix-blocks#40","title":"place blocks"}"#,
        r#"{"at_ms":2,"by":"ingest","actor":"tracker","kind":"imported","item":"gw","source":"github","upstream_id":"GetFelix/felix-gateway#31","title":"compare and set"}"#,
        r#"{"at_ms":3,"by":"gabriel","actor":"person","kind":"accepted","item":"blk"}"#,
        r#"{"at_ms":4,"by":"gabriel","actor":"person","kind":"linked","item":"blk","to":"gw"}"#,
    ];
    for (i, r) in recs.iter().enumerate() {
        assert!(f.apply(i as f64, r.as_bytes()), "{i}");
    }
    let blocked: serde_json::Value = serde_json::from_str(&f.blocked_json()).unwrap();
    assert_eq!(
        blocked,
        serde_json::json!([{
            "id": "blk",
            "title": "place blocks",
            "repo": { "source": "github", "repo": "GetFelix/felix-blocks" },
            "source": "github",
            "waits_on": [{
                "id": "gw",
                "title": "compare and set",
                "repo": { "source": "github", "repo": "GetFelix/felix-gateway" },
                "stage": "triage",
                "source": "github",
                "awaiting_release": false,
            }],
        }])
    );
    // A child item reports its root's repo.
    let child = [
        r#"{"at_ms":5,"by":"gabriel","actor":"person","kind":"proposed","item":"c1","parent":"blk","key":"k1","work":"split","title":"child"}"#,
        r#"{"at_ms":6,"by":"gabriel","actor":"person","kind":"linked","item":"c1","to":"gw"}"#,
    ];
    for (i, r) in child.iter().enumerate() {
        assert!(f.apply(10.0 + i as f64, r.as_bytes()), "{i}");
    }
    let rows: serde_json::Value = serde_json::from_str(&f.blocked_json()).unwrap();
    let c1 = rows.as_array().unwrap().iter().find(|r| r["id"] == "c1");
    assert_eq!(
        c1.map(|r| r["repo"].clone()),
        Some(serde_json::json!({ "source": "github", "repo": "GetFelix/felix-blocks" }))
    );
    let done = r#"{"at_ms":5,"by":"gabriel","actor":"person","kind":"closed","item":"gw","why":"completed"}"#;
    assert!(f.apply(20.0, done.as_bytes()));
    assert_eq!(f.blocked_json(), "[]");
}

fn feed(f: &mut Folder, at: &mut f64, recs: &[&str]) {
    for r in recs {
        assert!(f.apply(*at, r.as_bytes()), "{r}");
        *at += 1.0;
    }
}

fn blocked_ids(f: &Folder) -> Vec<String> {
    let rows: serde_json::Value = serde_json::from_str(&f.blocked_json()).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn blocked_lists_only_blocked_items() {
    let mut f = Folder::new();
    let mut at = 0.0;
    feed(
        &mut f,
        &mut at,
        &[
            r#"{"at_ms":1,"by":"ingest","actor":"tracker","kind":"imported","item":"dep","source":"github","upstream_id":"a/b#1","title":"dep"}"#,
            r#"{"at_ms":2,"by":"ingest","actor":"tracker","kind":"imported","item":"tri","source":"github","upstream_id":"a/b#2","title":"triage"}"#,
            r#"{"at_ms":3,"by":"ingest","actor":"tracker","kind":"imported","item":"hld","source":"github","upstream_id":"a/b#3","title":"held"}"#,
            r#"{"at_ms":4,"by":"ingest","actor":"tracker","kind":"imported","item":"blk","source":"github","upstream_id":"a/b#4","title":"blocked"}"#,
            r#"{"at_ms":5,"by":"gabriel","actor":"person","kind":"accepted","item":"hld"}"#,
            r#"{"at_ms":6,"by":"gabriel","actor":"person","kind":"accepted","item":"blk"}"#,
            r#"{"at_ms":7,"by":"w1","actor":"agent","kind":"claimed","item":"hld","attempt":1,"deadline_ms":1800000090000}"#,
            r#"{"at_ms":8,"by":"gabriel","actor":"person","kind":"linked","item":"tri","to":"dep"}"#,
            r#"{"at_ms":9,"by":"gabriel","actor":"person","kind":"linked","item":"hld","to":"dep"}"#,
            r#"{"at_ms":10,"by":"gabriel","actor":"person","kind":"linked","item":"blk","to":"dep"}"#,
        ],
    );
    let state: serde_json::Value = serde_json::from_str(&f.items_json()).unwrap();
    let stage = |id: &str| {
        state
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .map(|i| i["stage"].clone())
    };
    assert_eq!(stage("tri"), Some(serde_json::json!("triage")));
    assert_eq!(stage("hld"), Some(serde_json::json!("held")));
    assert_eq!(blocked_ids(&f), ["blk"]);
}

#[test]
fn a_merged_blocker_awaiting_its_release_says_so() {
    let mut f = Folder::new();
    let mut at = 0.0;
    feed(
        &mut f,
        &mut at,
        &[
            r#"{"at_ms":1,"by":"ingest","actor":"tracker","kind":"imported","item":"r1","source":"github","upstream_id":"a/b#1","title":"waits"}"#,
            r#"{"at_ms":2,"by":"ingest","actor":"tracker","kind":"imported","item":"r2","source":"github","upstream_id":"a/b#2","title":"ships"}"#,
            r#"{"at_ms":3,"by":"gabriel","actor":"person","kind":"accepted","item":"r1"}"#,
            r#"{"at_ms":4,"by":"w1","actor":"agent","kind":"claimed","item":"r1","attempt":1,"deadline_ms":1800000090000}"#,
            r#"{"at_ms":5,"by":"w1","actor":"agent","kind":"prerequisite","item":"r1","attempt":1,"prerequisite":"r2","key":"need","done_when":"released","cost_micros":0}"#,
            r#"{"at_ms":6,"by":"gabriel","actor":"person","kind":"closed","item":"r2","why":"merged"}"#,
        ],
    );
    let rows: serde_json::Value = serde_json::from_str(&f.blocked_json()).unwrap();
    assert_eq!(rows[0]["id"], "r1");
    assert_eq!(rows[0]["waits_on"][0]["awaiting_release"], true);
}
