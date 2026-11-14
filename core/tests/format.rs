//! The record format rules of `docs/upgrades.md`: records of the first format
//! read and fold as they always did, and a reader refuses a record of a newer
//! format instead of skipping it.

use kbl_core::codec::Codec;
use kbl_core::work::{
    Actor, FORMAT, FORMAT_V1, FORMAT_V2, FORMAT_V3, ReadError, WorkLogged, WorkRecord, read,
};
use kbl_core::work_fold::{State, fold};

/// Logs written before records carried a version.
const LOGS: [(&str, &str); 4] = [
    (
        "every_record",
        include_str!("../../wasm/golden/every_record.jsonl"),
    ),
    (
        "pre_prerequisite",
        include_str!("work_pre_prerequisite.jsonl"),
    ),
    ("pre_questions", include_str!("work_pre_questions.jsonl")),
    ("pre_review", include_str!("work_pre_review.jsonl")),
];

/// Every kind of the first format. Frozen: a new kind is not added here, it
/// gets a higher format in `WorkRecord::format`.
const V1_KINDS: [&str; 36] = [
    "imported",
    "proposed",
    "triaged",
    "accepted",
    "prerequisite",
    "shipped",
    "linked",
    "unlinked",
    "context",
    "claimed",
    "progress",
    "done",
    "released",
    "gave_up",
    "question",
    "answer",
    "decision",
    "finding",
    "review_verdict",
    "step",
    "edit",
    "human_input",
    "forked_from",
    "closed",
    "rejected",
    "updated",
    "left_scope",
    "reopened",
    "attempt_closed",
    "tier_changed",
    "edited",
    "budget_set",
    "pr_status",
    "source_stalled",
    "rules_set",
    "refused",
];

fn entries(log: &str) -> Vec<(u64, WorkLogged)> {
    log.lines()
        .enumerate()
        .map(|(i, l)| (i as u64, read(l.as_bytes()).unwrap()))
        .collect()
}

fn state_json(s: &State) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn the_first_format_is_frozen_at_one() {
    assert_eq!(FORMAT_V1, 1);
    const { assert!(FORMAT >= FORMAT_V1) };
}

#[test]
fn a_record_without_a_version_is_the_first_format_and_is_written_without_one() {
    for (name, log) in LOGS {
        for (i, line) in log.lines().enumerate() {
            let rec = read(line.as_bytes()).unwrap();
            assert_eq!(rec.v, FORMAT_V1, "{name}:{i}");
            assert_eq!(rec.record.format(), FORMAT_V1, "{name}:{i}");
            let again: serde_json::Value = serde_json::from_slice(&rec.encode()).unwrap();
            assert!(again.get("v").is_none(), "{name}:{i} gained a version");
            assert_eq!(
                again,
                serde_json::from_str::<serde_json::Value>(line).unwrap(),
                "{name}:{i}"
            );
        }
    }
}

/// `work_wire.jsonl` is every kind as the encoder writes it. Logged, each is
/// the envelope the tracker always wrote in front of those same bytes.
#[test]
fn a_first_format_record_is_logged_with_the_bytes_it_always_had() {
    let by = kbl_core::names::Key::new("w3").unwrap();
    for (i, line) in include_str!("work_wire.jsonl").lines().enumerate() {
        let record = WorkRecord::decode(line.as_bytes()).unwrap();
        let logged = WorkLogged::new(1_800_000_000_000, by.clone(), Actor::Agent, record);
        let want = format!(
            r#"{{"at_ms":1800000000000,"by":"w3","actor":"agent",{}"#,
            &line[1..]
        );
        assert_eq!(
            String::from_utf8(logged.encode()).unwrap(),
            want,
            "line {i}"
        );
        assert_eq!(read(want.as_bytes()).unwrap(), logged, "line {i}");
    }
}

#[test]
fn naming_the_first_format_changes_nothing() {
    for (name, log) in LOGS {
        let plain = fold(&entries(log));
        let named: String = log
            .lines()
            .map(|l| l.replacen('{', r#"{"v":1,"#, 1) + "\n")
            .collect();
        assert_eq!(
            state_json(&fold(&entries(&named))),
            state_json(&plain),
            "{name}"
        );
        // A log of the first format folds to a state with no format in it,
        // the JSON an older build wrote.
        assert_eq!(plain.format(), FORMAT_V1);
        let json: serde_json::Value = serde_json::to_value(&plain).unwrap();
        assert!(json.get("format").is_none(), "{name}");
    }
}

/// The two first-format kinds no stored log holds.
const MORE: [&str; 2] = [
    r#"{"at_ms":5,"by":"gab","actor":"person","kind":"tier_changed","item":"r1","tier":2}"#,
    r#"{"at_ms":5,"by":"gab","actor":"person","kind":"edited","item":"r1","title":"t"}"#,
];

#[test]
fn every_kind_of_the_first_format_still_needs_only_the_first_format() {
    let mut seen = Vec::new();
    let stored = LOGS.iter().flat_map(|(_, log)| log.lines());
    for line in stored.chain(MORE) {
        let rec = read(line.as_bytes()).unwrap();
        let kind = rec.record.kind();
        assert_ne!(kind, "unknown", "{line}");
        assert_eq!(rec.record.format(), FORMAT_V1, "{kind}");
        seen.push(kind);
    }
    seen.sort_unstable();
    seen.dedup();
    let mut frozen = V1_KINDS.to_vec();
    frozen.sort_unstable();
    // Every first-format kind is read from a stored record, and nothing else is.
    assert_eq!(seen, frozen);
    // A kind added later says which format introduced it.
    let raise = WorkRecord::FormatRaised { to: 2 };
    assert!(!V1_KINDS.contains(&raise.kind()));
    assert!(raise.format() > FORMAT_V1);
}

const NEWER: &str =
    r#"{"v":999,"at_ms":5,"by":"gab","actor":"person","kind":"accepted","item":"r1","tier":0}"#;

#[test]
fn a_record_of_a_newer_format_is_refused_not_skipped() {
    assert!(matches!(
        read(NEWER.as_bytes()),
        Err(ReadError::Newer { v: 999 })
    ));
    // The plain decoder cannot be used to get around it.
    assert!(WorkLogged::decode(NEWER.as_bytes()).is_err());
    // A kind this build has never heard of is refused the same way, where a
    // record of a readable format would be skipped as `unknown`.
    let kind = r#"{"v":999,"at_ms":5,"by":"gab","actor":"person","kind":"teleported","item":"r1"}"#;
    assert!(matches!(
        read(kind.as_bytes()),
        Err(ReadError::Newer { v: 999 })
    ));
    let old = r#"{"at_ms":5,"by":"gab","actor":"person","kind":"teleported","item":"r1"}"#;
    assert_eq!(read(old.as_bytes()).unwrap().record, WorkRecord::Unknown);
}

#[test]
fn a_version_that_is_not_a_format_is_malformed() {
    for v in ["0", "-1", "\"2\"", "1.5", "null", "4294967296"] {
        let line = NEWER.replacen("999", v, 1);
        assert!(
            matches!(read(line.as_bytes()), Err(ReadError::Malformed(_))),
            "v = {v}"
        );
    }
    assert!(matches!(read(b"not json"), Err(ReadError::Malformed(_))));
}

#[test]
fn an_unknown_field_in_a_readable_format_is_ignored() {
    let with = r#"{"at_ms":5,"by":"gab","actor":"person","kind":"accepted","item":"r1","tier":0,"later":true}"#;
    let without = with.replace(r#","later":true"#, "");
    assert_eq!(
        read(with.as_bytes()).unwrap(),
        read(without.as_bytes()).unwrap()
    );
}

#[test]
fn a_raise_is_written_with_the_format_it_raises_to() {
    let by = kbl_core::names::Key::new("gab").unwrap();
    let raise = WorkLogged::new(7, by, Actor::Person, WorkRecord::FormatRaised { to: 2 });
    assert_eq!(
        String::from_utf8(raise.encode()).unwrap(),
        r#"{"v":2,"at_ms":7,"by":"gab","actor":"person","kind":"format_raised","to":2}"#
    );
}

/// A raise past the newest format this build reads is refused and the log
/// keeps its format.
#[test]
fn a_raise_past_what_this_build_reads_is_refused() {
    let by = kbl_core::names::Key::new("gab").unwrap();
    let raise = WorkLogged::new(
        7,
        by,
        Actor::Person,
        WorkRecord::FormatRaised { to: FORMAT + 1 },
    );
    let mut state = fold(&entries(LOGS[1].1));
    let before = state.clone();
    assert_eq!(
        state.try_apply(100, &raise),
        Err(kbl_core::work_fold::Reject::Format)
    );
    assert_eq!(state, before);
    assert_eq!(state.format(), FORMAT_V1);
}

/// Every kind of the second format. Frozen like `V1_KINDS`.
const V2_KINDS: [&str; 1] = ["returned_to_scope"];

/// A log that raises to the second format and uses it. Its folded state is
/// pinned in `wasm/golden/format_2.state.json`.
const FORMAT_2_LOG: &str = include_str!("work_format_2.jsonl");

#[test]
fn the_second_format_is_two_and_names_its_kinds() {
    assert_eq!(FORMAT_V2, 2);
    const { assert!(FORMAT >= FORMAT_V2) };
    let mut seen: Vec<&str> = entries(FORMAT_2_LOG)
        .iter()
        .filter(|(_, r)| r.v == FORMAT_V2 && r.record.kind() != "format_raised")
        .map(|(_, r)| {
            assert_eq!(r.record.format(), FORMAT_V2, "{}", r.record.kind());
            r.record.kind()
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, V2_KINDS);
}

#[test]
fn a_second_format_record_is_written_with_its_format() {
    let by = kbl_core::names::Key::new("ingest").unwrap();
    let rec = WorkLogged::new(
        7,
        by,
        Actor::Tracker,
        WorkRecord::ReturnedToScope {
            item: kbl_core::names::ItemId::new("r1").unwrap(),
        },
    );
    assert_eq!(
        String::from_utf8(rec.encode()).unwrap(),
        r#"{"v":2,"at_ms":7,"by":"ingest","actor":"tracker","kind":"returned_to_scope","item":"r1"}"#
    );
}

/// The record before the raise is refused on replay, the one after it folds,
/// and a first-format record still folds after the raise.
#[test]
fn a_second_format_log_folds_after_its_raise() {
    let s = fold(&entries(FORMAT_2_LOG));
    assert_eq!(s.format(), FORMAT_V2);
    assert_eq!(
        s.rejections,
        vec![kbl_core::work_fold::Rejection {
            offset: 6,
            why: kbl_core::work_fold::Reject::Format
        }]
    );
    let r1 = kbl_core::names::ItemId::new("r1").unwrap();
    let r2 = kbl_core::names::ItemId::new("r2").unwrap();
    assert!(s.items[&r1].left_scope);
    assert!(!s.items[&r2].left_scope);
}

/// A log that raises to the third format and uses it: triage posts a
/// prerequisite for the item it triages. Its folded state is pinned in
/// `wasm/golden/format_3.state.json`.
const FORMAT_3_LOG: &str = include_str!("work_format_3.jsonl");

#[test]
fn the_third_format_is_three_and_is_a_prerequisite_from_triage() {
    assert_eq!(FORMAT_V3, 3);
    const { assert!(FORMAT >= FORMAT_V3) };
    let third: Vec<_> = entries(FORMAT_3_LOG)
        .into_iter()
        .filter(|(_, r)| r.v == FORMAT_V3 && r.record.kind() != "format_raised")
        .collect();
    assert!(!third.is_empty());
    for (_, r) in third {
        assert_eq!(r.record.format(), FORMAT_V3);
        assert!(matches!(
            r.record,
            WorkRecord::Prerequisite {
                triage: Some(_),
                ..
            }
        ));
    }
    // The same kind without `triage` is still the first format.
    let plain = entries(FORMAT_3_LOG)
        .into_iter()
        .find(|(_, r)| matches!(r.record, WorkRecord::Prerequisite { triage: None, .. }))
        .unwrap();
    assert_eq!((plain.1.v, plain.1.record.format()), (FORMAT_V1, FORMAT_V1));
}

/// The record before the raise is refused on replay and the ones after it
/// fold: the item under triage waits on both.
#[test]
fn a_third_format_log_folds_after_its_raise() {
    let s = fold(&entries(FORMAT_3_LOG));
    assert_eq!(s.format(), FORMAT_V3);
    assert_eq!(
        s.rejections,
        vec![kbl_core::work_fold::Rejection {
            offset: 5,
            why: kbl_core::work_fold::Reject::Format
        }]
    );
    let id = |s: &str| kbl_core::names::ItemId::new(s).unwrap();
    assert!(s.edges.contains(&(id("r1"), id("g1"))));
    assert!(s.edges.contains(&(id("r1"), id("r2"))));
    // Both posts were counted on the triage claim, which went on to finish.
    assert_eq!(s.items[&id("t1")].paths[0].posts, 2);
    assert!(!s.items.contains_key(&id("g0")));
}
