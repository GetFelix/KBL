//! The record format rules of `docs/upgrades.md`: records of the first format
//! read and fold as they always did, and a reader refuses a record of a newer
//! format instead of skipping it.

use agents_core::codec::Codec;
use agents_core::work::{Actor, FORMAT, FORMAT_V1, ReadError, WorkLogged, WorkRecord, read};
use agents_core::work_fold::{State, fold};

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
    let by = agents_core::names::Key::new("w3").unwrap();
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

#[test]
fn every_kind_of_the_first_format_still_needs_only_the_first_format() {
    let mut seen = Vec::new();
    for (_, log) in LOGS {
        for (_, rec) in entries(log) {
            let kind = rec.record.kind();
            assert!(
                V1_KINDS.contains(&kind),
                "{kind} is not a first-format kind"
            );
            assert_eq!(rec.record.format(), FORMAT_V1, "{kind}");
            seen.push(kind);
        }
    }
    // A kind added later must say which format introduced it.
    let raise = WorkRecord::FormatRaised { to: 2 };
    assert!(!V1_KINDS.contains(&raise.kind()));
    assert!(raise.format() > FORMAT_V1);
    seen.sort_unstable();
    seen.dedup();
    assert!(seen.len() >= 34, "the logs cover {} kinds", seen.len());
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
    let by = agents_core::names::Key::new("gab").unwrap();
    let raise = WorkLogged::new(7, by, Actor::Person, WorkRecord::FormatRaised { to: 2 });
    assert_eq!(
        String::from_utf8(raise.encode()).unwrap(),
        r#"{"v":2,"at_ms":7,"by":"gab","actor":"person","kind":"format_raised","to":2}"#
    );
}

/// This build reads one format, so there is nothing to raise to yet: the
/// record is refused and the log stays at the first format.
#[test]
fn a_raise_past_what_this_build_reads_is_refused() {
    let by = agents_core::names::Key::new("gab").unwrap();
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
        Err(agents_core::work_fold::Reject::Format)
    );
    assert_eq!(state, before);
    assert_eq!(state.format(), FORMAT);
}
