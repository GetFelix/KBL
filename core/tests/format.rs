//! The record format rules of `docs/upgrades.md`: records of the first format
//! read and fold as they always did, and a reader refuses a record of a newer
//! format instead of skipping it.

use kbl_core::codec::Codec;
use kbl_core::work::{
    Actor, FORMAT, FORMAT_V1, FORMAT_V2, FORMAT_V3, FORMAT_V4, FORMAT_V5, FORMAT_V6, FORMAT_V7,
    FORMAT_V8, FORMAT_V9, FORMAT_V10, FORMAT_V11, FORMAT_V12, FORMAT_V13, FORMAT_V14, FORMAT_V15,
    FORMAT_V16, FORMAT_V17, FORMAT_V18, FORMAT_V19, FORMAT_V20, ReadError, WorkLogged, WorkRecord,
    read,
};
use kbl_core::work_fold::{NeedsYou, Reject, Rejection, Stage, State, fold};

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

/// A log that raises to the fourth format: a redo waits on an item whose
/// request has a failed check. Its folded state is pinned in
/// `wasm/golden/format_4.state.json`.
const FORMAT_4_LOG: &str = include_str!("work_format_4.jsonl");

/// Format 4 adds no record: only the raise is stamped 4.
#[test]
fn the_fourth_format_is_four_and_adds_no_record() {
    assert_eq!(FORMAT_V4, 4);
    const { assert!(FORMAT >= FORMAT_V4) };
    for (_, r) in entries(FORMAT_4_LOG) {
        let raise = r.record.kind() == "format_raised";
        assert_eq!(r.v, if raise { FORMAT_V4 } else { FORMAT_V1 });
        assert_eq!(r.record.format(), r.v);
    }
}

/// Before the raise the redo waits for green and its claim is refused; after
/// it the item is Ready and the same claim folds.
#[test]
fn a_redo_of_a_red_item_is_handed_out_once_the_log_is_at_format_4() {
    let log = entries(FORMAT_4_LOG);
    let r1 = kbl_core::names::ItemId::new("r1").unwrap();
    let before = fold(&log[..9]);
    assert_eq!(before.needs_you(&r1), Some(NeedsYou::CheckFailed));
    assert_eq!(before.stage(&r1), Some(Stage::NeedsYou));
    let raised = fold(&log[..11]);
    assert_eq!(raised.format(), FORMAT_V4);
    assert_eq!(raised.needs_you(&r1), None);
    assert_eq!(raised.stage(&r1), Some(Stage::Ready));

    let s = fold(&log[..14]);
    assert_eq!(
        s.rejections,
        vec![Rejection {
            offset: 9,
            why: Reject::NotReady
        }]
    );
    // w2's claim was released and w3 retried the same attempt.
    let it = &s.items[&r1];
    assert_eq!(
        it.hold.as_ref().map(|h| (h.by.as_str(), h.attempt)),
        Some(("w3", 2))
    );
    assert_eq!(it.paths[1].tries, 2);
    assert!(it.paths[1].forked_from.is_some());
}

/// r2's redo pushed, its checks failed again and its claim lapsed. The note
/// the redo named is older than the new failure, so the retry waits; a new
/// note on the item frees it without another redo.
#[test]
fn a_redo_that_goes_red_again_waits_for_a_new_note() {
    let log = entries(FORMAT_4_LOG);
    let r2 = kbl_core::names::ItemId::new("r2").unwrap();
    let lapsed = fold(&log[..28]);
    assert_eq!(lapsed.items[&r2].red_since, Some(26));
    assert_eq!(lapsed.needs_you(&r2), Some(NeedsYou::CheckFailed));
    let noted = fold(&log[..30]);
    assert_eq!(noted.needs_you(&r2), None);
    assert_eq!(noted.stage(&r2), Some(Stage::Ready));

    let s = fold(&log);
    assert_eq!(
        s.rejections,
        vec![
            Rejection {
                offset: 9,
                why: Reject::NotReady
            },
            Rejection {
                offset: 28,
                why: Reject::NotReady
            }
        ]
    );
    let it = &s.items[&r2];
    assert_eq!(
        it.hold.as_ref().map(|h| (h.by.as_str(), h.attempt)),
        Some(("w4", 2))
    );
    assert_eq!(it.paths[1].tries, 2);
}

/// A raise to format 3 is not enough: the redo still waits for green.
#[test]
fn a_redo_of_a_red_item_waits_at_format_3() {
    let raise4 =
        r#"{"v":4,"at_ms":1100,"by":"gabriel","actor":"person","kind":"format_raised","to":4}"#;
    let raise3 =
        r#"{"v":3,"at_ms":1100,"by":"gabriel","actor":"person","kind":"format_raised","to":3}"#;
    assert!(FORMAT_4_LOG.contains(raise4));
    let log = entries(&FORMAT_4_LOG.replace(raise4, raise3));
    let s = fold(&log[..12]);
    assert_eq!(s.format(), FORMAT_V3);
    let r1 = kbl_core::names::ItemId::new("r1").unwrap();
    assert_eq!(s.needs_you(&r1), Some(NeedsYou::CheckFailed));
    assert!(s.items[&r1].hold.is_none());
    assert_eq!(s.rejections.len(), 2);
    assert!(s.rejections.iter().all(|r| r.why == Reject::NotReady));
}

/// A log that raises to the fifth format: an agent waits on the release of
/// work that is already merged, and a later prerequisite on the same link asks
/// only for the merge. Its folded state is pinned in
/// `wasm/golden/format_5.state.json`.
const FORMAT_5_LOG: &str = include_str!("work_format_5.jsonl");

/// Format 5 adds no record: only the raise is stamped 5.
#[test]
fn the_fifth_format_is_five_and_adds_no_record() {
    assert_eq!(FORMAT_V5, 5);
    const { assert!(FORMAT >= FORMAT_V5) };
    for (_, r) in entries(FORMAT_5_LOG) {
        if r.record.kind() == "format_raised" {
            assert_eq!(r.v, FORMAT_V5);
        } else {
            assert!(r.v < FORMAT_V5);
        }
        assert_eq!(r.record.format(), r.v);
    }
}

/// Before the raise a merged item with no release is refused as `closed`;
/// after it the same prerequisite folds and the item waits for the release.
#[test]
fn a_merged_item_with_no_release_is_a_prerequisite_from_format_5() {
    let log = entries(FORMAT_5_LOG);
    let r1 = kbl_core::names::ItemId::new("r1").unwrap();
    let g1 = kbl_core::names::ItemId::new("g1").unwrap();
    let before = fold(&log[..7]);
    assert_eq!(
        before.rejections,
        vec![Rejection {
            offset: 6,
            why: Reject::Closed
        }]
    );
    assert_eq!(before.stage(&r1), Some(Stage::Held));

    let waiting = fold(&log[..9]);
    assert_eq!(waiting.format(), FORMAT_V5);
    assert_eq!(waiting.rejections.len(), 1);
    assert!(waiting.release_edges.contains(&(r1.clone(), g1.clone())));
    assert_eq!(waiting.stage(&r1), Some(Stage::Blocked));
    let shipped = fold(&log[..10]);
    assert_eq!(shipped.stage(&r1), Some(Stage::Ready));
}

/// A later prerequisite asking only for the merge keeps the earlier ask for
/// the release, so r2 waits after g2 merges.
#[test]
fn a_later_merged_ask_keeps_the_release_ask_from_format_5() {
    let s = fold(&entries(FORMAT_5_LOG));
    let r2 = kbl_core::names::ItemId::new("r2").unwrap();
    let g2 = kbl_core::names::ItemId::new("g2").unwrap();
    assert_eq!(s.rejections.len(), 1);
    assert!(s.release_edges.contains(&(r2.clone(), g2)));
    assert_eq!(s.stage(&r2), Some(Stage::Blocked));
}

/// A raise to format 4 is not enough: the merged item is still refused and
/// the later merged ask replaces the release ask.
#[test]
fn format_4_keeps_the_old_prerequisite_rules() {
    let raise5 =
        r#"{"v":5,"at_ms":1070,"by":"gabriel","actor":"person","kind":"format_raised","to":5}"#;
    let raise4 =
        r#"{"v":4,"at_ms":1070,"by":"gabriel","actor":"person","kind":"format_raised","to":4}"#;
    assert!(FORMAT_5_LOG.contains(raise5));
    let s = fold(&entries(&FORMAT_5_LOG.replace(raise5, raise4)));
    assert_eq!(s.format(), FORMAT_V4);
    let id = |s: &str| kbl_core::names::ItemId::new(s).unwrap();
    assert_eq!(
        s.rejections,
        vec![
            Rejection {
                offset: 6,
                why: Reject::Closed
            },
            Rejection {
                offset: 8,
                why: Reject::Closed
            }
        ]
    );
    assert!(s.release_edges.is_empty());
    assert_eq!(s.stage(&id("r2")), Some(Stage::Ready));
}

/// The format 5 log with `more` appended, one record per line.
fn format_5_with(more: &str) -> State {
    fold(&entries(&format!("{FORMAT_5_LOG}{more}")))
}

fn id(s: &str) -> kbl_core::names::ItemId {
    kbl_core::names::ItemId::new(s).unwrap()
}

/// Re-opening a prerequisite keeps every release wait: dropping it would turn
/// a `released` ask into `merged` and free the waiter on unreleased work. The
/// reopen clears the release, so the waiter waits for the next one. Other
/// edges survive, and the rule is the same in a format 4 log.
#[test]
fn a_reopened_prerequisite_keeps_its_release_waits() {
    let raise5 =
        r#"{"v":5,"at_ms":1070,"by":"gabriel","actor":"person","kind":"format_raised","to":5}"#;
    let raise4 =
        r#"{"v":4,"at_ms":1070,"by":"gabriel","actor":"person","kind":"format_raised","to":4}"#;
    // Up to the release ask on g2, while g2 is still open.
    let head: String = FORMAT_5_LOG
        .lines()
        .take(17)
        .map(|l| format!("{l}\n"))
        .collect();
    let more = r#"{"at_ms":1300,"by":"gabriel","actor":"person","kind":"linked","item":"r2","to":"g1"}
{"at_ms":1310,"by":"gabriel","actor":"person","kind":"closed","item":"g2","why":"merged"}
{"at_ms":1320,"by":"gabriel","actor":"person","kind":"reopened","item":"g2"}
{"at_ms":1330,"by":"gabriel","actor":"person","kind":"accepted","item":"g2","tier":1}
{"at_ms":1340,"by":"gabriel","actor":"person","kind":"closed","item":"g2","why":"merged"}
"#;
    let shipped = r#"{"at_ms":1350,"by":"gabriel","actor":"person","kind":"shipped","item":"g2","release":"0.5.0"}
"#;
    for log in [head.clone(), head.replace(raise5, raise4)] {
        let s = fold(&entries(&format!("{log}{more}")));
        let (r2, g2, g1) = (id("r2"), id("g2"), id("g1"));
        assert!(s.release_edges.contains(&(r2.clone(), g2.clone())));
        assert!(s.edges.contains(&(r2.clone(), g1)), "{:?}", s.rejections);
        assert_eq!(s.stage(&r2), Some(Stage::Blocked));
        let s = fold(&entries(&format!("{log}{more}{shipped}")));
        assert_ne!(s.stage(&r2), Some(Stage::Blocked));
    }
}

/// Items for the format 5 refusal tests: r3 is held by w3, g3 dismissed, g4
/// rejected, g5 merged, g6 completed.
const FORMAT_5_MORE: &str = r#"{"at_ms":1300,"by":"gabriel","actor":"person","kind":"imported","item":"r3","source":"github","upstream_id":"GetFelix/felix-blocks#3","title":"undo"}
{"at_ms":1301,"by":"gabriel","actor":"person","kind":"accepted","item":"r3","tier":1}
{"at_ms":1302,"by":"gabriel","actor":"person","kind":"imported","item":"g3","source":"github","upstream_id":"GetFelix/felix-gateway#9","title":"dismissed"}
{"at_ms":1303,"by":"gabriel","actor":"person","kind":"accepted","item":"g3","tier":1}
{"at_ms":1304,"by":"gabriel","actor":"person","kind":"closed","item":"g3","why":"dismissed"}
{"at_ms":1305,"by":"gabriel","actor":"person","kind":"imported","item":"g4","source":"github","upstream_id":"GetFelix/felix-gateway#10","title":"rejected"}
{"at_ms":1306,"by":"gabriel","actor":"person","kind":"rejected","item":"g4"}
{"at_ms":1307,"by":"gabriel","actor":"person","kind":"imported","item":"g5","source":"github","upstream_id":"GetFelix/felix-gateway#11","title":"merged"}
{"at_ms":1308,"by":"gabriel","actor":"person","kind":"accepted","item":"g5","tier":1}
{"at_ms":1309,"by":"gabriel","actor":"person","kind":"closed","item":"g5","why":"merged"}
{"at_ms":1310,"by":"gabriel","actor":"person","kind":"imported","item":"g6","source":"github","upstream_id":"GetFelix/felix-gateway#12","title":"completed"}
{"at_ms":1311,"by":"gabriel","actor":"person","kind":"accepted","item":"g6","tier":1}
{"at_ms":1312,"by":"gabriel","actor":"person","kind":"closed","item":"g6","why":"completed"}
{"at_ms":1313,"by":"w3","actor":"agent","kind":"claimed","item":"r3","attempt":1,"deadline_ms":1800000090000}
"#;

/// w3 asks on r3 to wait for `on` until `done_when`.
fn ask_from_r3(at: u64, on: &str, done_when: &str) -> String {
    format!(
        r#"{{"at_ms":{at},"by":"w3","actor":"agent","kind":"prerequisite","item":"r3","attempt":1,"prerequisite":"{on}","key":"need-{on}","done_when":"{done_when}","cost_micros":0}}
"#
    )
}

/// Only finished, unreleased work is worth a release wait: a released,
/// dismissed or rejected item is refused, and so is a `merged` ask on merged
/// work.
#[test]
fn closed_prerequisites_are_still_refused_at_format_5() {
    let base = FORMAT_5_LOG.lines().count() + FORMAT_5_MORE.lines().count();
    for (on, done_when) in [
        ("g1", "released"),
        ("g3", "released"),
        ("g4", "released"),
        ("g5", "merged"),
    ] {
        let s = format_5_with(&format!(
            "{FORMAT_5_MORE}{}",
            ask_from_r3(1400, on, done_when)
        ));
        assert_eq!(
            s.rejections.last(),
            Some(&Rejection {
                offset: base as u64,
                why: Reject::Closed
            }),
            "{on} {done_when}"
        );
        assert_eq!(s.rejections.len(), 2, "{on} {done_when}");
    }
}

/// Completed work waits for a release like merged work, until a person ships
/// it or unlinks the waiter.
#[test]
fn a_completed_item_is_a_release_prerequisite_until_unlinked_at_format_5() {
    let asked = format!("{FORMAT_5_MORE}{}", ask_from_r3(1400, "g6", "released"));
    let s = format_5_with(&asked);
    assert_eq!(s.rejections.len(), 1, "{:?}", s.rejections);
    assert!(s.release_edges.contains(&(id("r3"), id("g6"))));
    assert_eq!(s.stage(&id("r3")), Some(Stage::Blocked));

    let unlinked = r#"{"at_ms":1410,"by":"gabriel","actor":"person","kind":"unlinked","item":"r3","to":"g6"}
"#;
    let s = format_5_with(&format!("{asked}{unlinked}"));
    assert!(!s.release_edges.contains(&(id("r3"), id("g6"))));
    assert_ne!(s.stage(&id("r3")), Some(Stage::Blocked));
}

/// A log that raises to the sixth format: ingest opened an issue for each of
/// two prerequisites, one a person accepted and one the tracker did. Its folded
/// state is pinned in `wasm/golden/format_6.state.json`.
const FORMAT_6_LOG: &str = include_str!("work_format_6.jsonl");

const RAISE_6: &str =
    r#"{"v":6,"at_ms":1110,"by":"gabriel","actor":"person","kind":"format_raised","to":6}"#;

/// Format 6 adds no record: only the raise is stamped 6.
#[test]
fn the_sixth_format_is_six_and_adds_no_record() {
    assert_eq!(FORMAT_V6, 6);
    const { assert!(FORMAT >= FORMAT_V6) };
    assert!(FORMAT_6_LOG.contains(RAISE_6));
    for (_, r) in entries(FORMAT_6_LOG) {
        if r.record.kind() == "format_raised" {
            assert_eq!(r.v, FORMAT_V6);
        } else {
            assert!(r.v < FORMAT_V6);
        }
        assert_eq!(r.record.format(), r.v);
    }
}

fn imported_as<'a>(s: &'a State, item: &str) -> Vec<&'a str> {
    s.imported()
        .filter(|(_, _, i)| i.as_str() == item)
        .map(|(_, up, _)| up)
        .collect()
}

/// Before the raise `imported` on an existing item is a duplicate. After it,
/// a prerequisite a person accepted takes the issue once. An acceptance from
/// before the raise or by the tracker is not enough.
#[test]
fn an_accepted_prerequisite_takes_its_upstream_issue_from_format_6() {
    let log = entries(FORMAT_6_LOG);
    let before = fold(&log[..11]);
    assert_eq!(
        before.rejections,
        vec![Rejection {
            offset: 10,
            why: Reject::Duplicate
        }]
    );
    assert!(imported_as(&before, "g1").is_empty());

    let s = fold(&log);
    assert_eq!(s.format(), FORMAT_V6);
    let rejected: Vec<(u64, Reject)> = s.rejections.iter().map(|r| (r.offset, r.why)).collect();
    assert_eq!(
        rejected,
        vec![
            (10, Reject::Duplicate),
            (12, Reject::ActorNotAllowed),
            (15, Reject::Duplicate),
            (16, Reject::ActorNotAllowed),
            (29, Reject::NotAProposal)
        ]
    );
    assert_eq!(imported_as(&s, "g1"), vec!["GetFelix/felix-gateway#40"]);
    assert!(imported_as(&s, "g2").is_empty());
    assert!(s.items[&id("g1")].closed_by_sync);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    assert_eq!(s.stage(&id("r2")), Some(Stage::Blocked));
}

/// A format 5 log keeps `imported` on an existing item a duplicate, so the
/// prerequisite stays unimported.
#[test]
fn format_5_refuses_an_issue_for_an_existing_item() {
    let raise5 =
        r#"{"v":5,"at_ms":1110,"by":"gabriel","actor":"person","kind":"format_raised","to":5}"#;
    let s = fold(&entries(&FORMAT_6_LOG.replace(RAISE_6, raise5)));
    assert_eq!(s.format(), FORMAT_V5);
    assert!(imported_as(&s, "g1").is_empty());
    let rejected: Vec<(u64, Reject)> = s.rejections.iter().map(|r| (r.offset, r.why)).collect();
    assert_eq!(
        rejected,
        [10, 12, 14, 15, 16, 29]
            .map(|o| (o, Reject::Duplicate))
            .to_vec()
    );
}

/// The issue must be in the repo the prerequisite targets, on an open item;
/// a person accepting the tracker-accepted one lets it take its issue.
#[test]
fn the_issue_must_match_the_target_on_an_open_item_at_format_6() {
    let more = r#"{"at_ms":1200,"by":"gabriel","actor":"person","kind":"accepted","item":"g2","tier":1}
{"at_ms":1210,"by":"ingest","actor":"tracker","kind":"imported","item":"g2","source":"github","upstream_id":"GetFelix/felix-blocks#43","title":"gateway rate limits"}
{"at_ms":1220,"by":"ingest","actor":"tracker","kind":"imported","item":"g2","source":"gitlab","upstream_id":"GetFelix/felix-gateway#43","title":"gateway rate limits"}
{"at_ms":1230,"by":"ingest","actor":"tracker","kind":"imported","item":"g2","source":"github","upstream_id":"GetFelix/felix-gateway#43","title":"gateway rate limits"}
{"at_ms":1240,"by":"ingest","actor":"tracker","kind":"imported","item":"r1","source":"github","upstream_id":"GetFelix/felix-blocks#44","title":"swap blocks atomically"}
"#;
    let s = fold(&entries(&format!("{FORMAT_6_LOG}{more}")));
    let rejected: Vec<(u64, Reject)> = s.rejections[5..]
        .iter()
        .map(|r| (r.offset, r.why))
        .collect();
    assert_eq!(
        rejected,
        vec![
            (31, Reject::Duplicate),
            (32, Reject::Duplicate),
            (34, Reject::Duplicate)
        ]
    );
    assert_eq!(imported_as(&s, "g2"), vec!["GetFelix/felix-gateway#43"]);

    let closed = r#"{"at_ms":1200,"by":"gabriel","actor":"person","kind":"accepted","item":"g2","tier":1}
{"at_ms":1210,"by":"gabriel","actor":"person","kind":"closed","item":"g2","why":"dismissed"}
{"at_ms":1220,"by":"ingest","actor":"tracker","kind":"imported","item":"g2","source":"github","upstream_id":"GetFelix/felix-gateway#43","title":"gateway rate limits"}
"#;
    let s = fold(&entries(&format!("{FORMAT_6_LOG}{closed}")));
    assert_eq!(s.rejections.last().map(|r| r.why), Some(Reject::Closed));
    assert!(imported_as(&s, "g2").is_empty());
}

/// A deferred review finding targets the reviewed repo and a person may accept
/// it, but it is not a prerequisite: ingest never opens an issue for it.
#[test]
fn a_deferred_finding_is_never_opened_upstream_at_format_6() {
    let s = fold(&entries(FORMAT_6_LOG));
    let deferred = s
        .items
        .values()
        .find(|it| it.finding.is_some())
        .expect("the golden defers a finding");
    assert!(deferred.accepted_by_person && deferred.target.is_some());
    assert!(!deferred.is_prerequisite());
    assert!(imported_as(&s, deferred.id.as_str()).is_empty());
    assert_eq!(
        s.rejections.last().map(|r| r.why),
        Some(Reject::NotAProposal)
    );
    let unopened: Vec<&str> = s
        .unopened_prerequisites()
        .iter()
        .map(|(it, _)| it.id.as_str())
        .collect();
    assert!(!unopened.contains(&deferred.id.as_str()));
}

/// GitHub names are case-insensitive, so an issue in `getfelix/...` matches
/// a prerequisite that targets `GetFelix/...`.
#[test]
fn the_target_repo_matches_without_case_at_format_6() {
    let more = r#"{"at_ms":1300,"by":"gabriel","actor":"person","kind":"accepted","item":"g2","tier":1}
{"at_ms":1310,"by":"ingest","actor":"tracker","kind":"imported","item":"g2","source":"github","upstream_id":"getfelix/FELIX-gateway#43","title":"gateway rate limits"}
"#;
    let s = fold(&entries(&format!("{FORMAT_6_LOG}{more}")));
    assert_eq!(imported_as(&s, "g2"), vec!["getfelix/FELIX-gateway#43"]);
}

/// A log that raises to the seventh format and flags items. Its folded state
/// is pinned in `wasm/golden/format_7.state.json`; `work_replay.rs` checks it.
const FORMAT_7_LOG: &str = include_str!("work_format_7.jsonl");

#[test]
fn the_seventh_format_is_seven_and_names_its_kinds() {
    assert_eq!(FORMAT_V7, 7);
    const { assert!(FORMAT >= FORMAT_V7) };
    let mut seen: Vec<&str> = entries(FORMAT_7_LOG)
        .iter()
        .filter(|(_, r)| r.v == FORMAT_V7 && r.record.kind() != "format_raised")
        .map(|(_, r)| {
            assert_eq!(r.record.format(), FORMAT_V7, "{}", r.record.kind());
            r.record.kind()
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, ["flagged"]);
}

#[test]
fn a_flagged_record_is_written_with_its_format() {
    let rec = WorkLogged::new(
        7,
        kbl_core::names::Key::new("gabriel").unwrap(),
        Actor::Person,
        WorkRecord::Flagged {
            item: kbl_core::names::ItemId::new("r1").unwrap(),
            flagged: true,
        },
    );
    assert_eq!(
        String::from_utf8(rec.encode()).unwrap(),
        r#"{"v":7,"at_ms":7,"by":"gabriel","actor":"person","kind":"flagged","item":"r1","flagged":true}"#
    );
}

/// Usage, a floor or a price is format 8: a format 7 reader would ignore the
/// fields and fold a different spend. A report without them stays format 1.
#[test]
fn usage_floors_and_prices_are_format_8() {
    use kbl_core::names::{ItemId, Key};
    use kbl_core::work::{PriceUnit, Usage};
    assert_eq!(FORMAT_V8, 8);
    const { assert!(FORMAT >= FORMAT_V8) };
    let done = |usage: Vec<Usage>, floor_micros| WorkRecord::Done {
        item: ItemId::new("r1").unwrap(),
        attempt: 1,
        cost_micros: 0,
        usage,
        floor_micros,
    };
    let aic = Usage::Aic {
        model: "m".into(),
        milli: 1,
    };
    assert_eq!(done(vec![], 0).format(), FORMAT_V1);
    assert_eq!(done(vec![aic], 0).format(), FORMAT_V8);
    assert_eq!(done(vec![], 1).format(), FORMAT_V8);
    let price = WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Aic,
        price_micros: 1,
        effective_from_ms: None,
        as_of: None,
    };
    assert_eq!(price.format(), FORMAT_V8);
    let rec = WorkLogged::new(7, Key::new("gabriel").unwrap(), Actor::Person, price);
    assert_eq!(
        String::from_utf8(rec.encode()).unwrap(),
        r#"{"v":8,"at_ms":7,"by":"gabriel","actor":"person","kind":"price_set","model":"m","unit":"aic","price_micros":1}"#
    );
}

/// The fold reads `as_of` on a price, so a format 8 reader would fold it
/// differently: a price with it is format 9, one without stays format 8.
#[test]
fn a_price_with_as_of_is_format_9() {
    use kbl_core::names::Key;
    use kbl_core::work::PriceUnit;
    assert_eq!(FORMAT_V9, 9);
    const { assert!(FORMAT >= FORMAT_V9) };
    let price = |as_of: Option<&str>| WorkRecord::PriceSet {
        model: "m".into(),
        unit: PriceUnit::Aic,
        price_micros: 1,
        effective_from_ms: None,
        as_of: as_of.map(Into::into),
    };
    assert_eq!(price(None).format(), FORMAT_V8);
    assert_eq!(price(Some("2026-11-15")).format(), FORMAT_V9);
    let rec = WorkLogged::new(
        7,
        Key::new("gabriel").unwrap(),
        Actor::Person,
        price(Some("2026-11-15")),
    );
    assert!(
        String::from_utf8(rec.encode())
            .unwrap()
            .starts_with(r#"{"v":9,"#)
    );
}

/// A log that raises to the tenth format and takes the card actions. Its
/// folded state is pinned in `wasm/golden/format_10.state.json`; `work_replay.rs`
/// checks each step.
const FORMAT_10_LOG: &str = include_str!("work_format_10.jsonl");

#[test]
fn the_tenth_format_is_ten_and_names_its_kinds() {
    assert_eq!(FORMAT_V10, 10);
    const { assert!(FORMAT >= FORMAT_V10) };
    let mut seen: Vec<&str> = entries(FORMAT_10_LOG)
        .iter()
        .filter(|(_, r)| r.v == FORMAT_V10 && r.record.kind() != "format_raised")
        .map(|(_, r)| {
            assert_eq!(r.record.format(), FORMAT_V10, "{}", r.record.kind());
            r.record.kind()
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, ["parked", "retried", "snoozed", "unblocked"]);
}

#[test]
fn a_card_action_is_written_with_its_format() {
    let item = |s: &str| kbl_core::names::ItemId::new(s).unwrap();
    let write = |record| {
        String::from_utf8(
            WorkLogged::new(
                7,
                kbl_core::names::Key::new("gabriel").unwrap(),
                Actor::Person,
                record,
            )
            .encode(),
        )
        .unwrap()
    };
    assert_eq!(
        write(WorkRecord::Snoozed {
            item: item("r1"),
            until_ms: 9
        }),
        r#"{"v":10,"at_ms":7,"by":"gabriel","actor":"person","kind":"snoozed","item":"r1","until_ms":9}"#
    );
    assert_eq!(
        write(WorkRecord::Parked {
            item: item("r1"),
            parked: true
        }),
        r#"{"v":10,"at_ms":7,"by":"gabriel","actor":"person","kind":"parked","item":"r1","parked":true}"#
    );
}

/// A log that raises to the eleventh format and takes a person's moves on a
/// pull request and on the summary. Its folded state is pinned in
/// `wasm/golden/format_11.state.json`; `work_replay.rs` checks each step.
const FORMAT_11_LOG: &str = include_str!("work_format_11.jsonl");

#[test]
fn the_eleventh_format_is_eleven_and_names_its_kinds() {
    assert_eq!(FORMAT_V11, 11);
    const { assert!(FORMAT >= FORMAT_V11) };
    let mut seen: Vec<&str> = entries(FORMAT_11_LOG)
        .iter()
        .filter(|(_, r)| r.v == FORMAT_V11 && r.record.kind() != "format_raised")
        .map(|(_, r)| {
            assert_eq!(r.record.format(), FORMAT_V11, "{}", r.record.kind());
            r.record.kind()
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen,
        [
            "changes_requested",
            "merge_marked",
            "review_requested",
            "summary_rated"
        ]
    );
}

#[test]
fn a_review_action_is_written_with_its_format() {
    let item = |s: &str| kbl_core::names::ItemId::new(s).unwrap();
    let write = |record| {
        String::from_utf8(
            WorkLogged::new(
                7,
                kbl_core::names::Key::new("gabriel").unwrap(),
                Actor::Person,
                record,
            )
            .encode(),
        )
        .unwrap()
    };
    assert_eq!(
        write(WorkRecord::SummaryRated {
            item: item("r1"),
            attempt: 2,
            accurate: true
        }),
        r#"{"v":11,"at_ms":7,"by":"gabriel","actor":"person","kind":"summary_rated","item":"r1","attempt":2,"accurate":true}"#
    );
    assert_eq!(
        write(WorkRecord::ReviewRequested {
            item: item("r1"),
            reviewer: "kim".into()
        }),
        r#"{"v":11,"at_ms":7,"by":"gabriel","actor":"person","kind":"review_requested","item":"r1","reviewer":"kim"}"#
    );
    assert_eq!(
        write(WorkRecord::MergeMarked { item: item("r1") }),
        r#"{"v":11,"at_ms":7,"by":"gabriel","actor":"person","kind":"merge_marked","item":"r1"}"#
    );
}

/// A log that raises to the twelfth format and assigns, takes and steers.
/// Its folded state is pinned in `wasm/golden/format_12.state.json`;
/// `work_replay.rs` checks each step.
const FORMAT_12_LOG: &str = include_str!("work_format_12.jsonl");

#[test]
fn the_twelfth_format_is_twelve_and_names_its_kinds() {
    assert_eq!(FORMAT_V12, 12);
    const { assert!(FORMAT >= FORMAT_V12) };
    let mut seen: Vec<&str> = entries(FORMAT_12_LOG)
        .iter()
        .filter(|(_, r)| r.v == FORMAT_V12 && r.record.kind() != "format_raised")
        .map(|(_, r)| {
            assert_eq!(r.record.format(), FORMAT_V12, "{}", r.record.kind());
            r.record.kind()
        })
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, ["reassigned", "steered", "taken"]);
}

/// A reader at format 11 refuses any `v` above 11, so it stops at the raise
/// to 12 and at every record of it, instead of folding them wrong.
#[test]
fn an_assignee_record_and_the_raise_to_it_stop_a_format_11_reader() {
    let item = |s: &str| kbl_core::names::ItemId::new(s).unwrap();
    let write = |record| {
        String::from_utf8(
            WorkLogged::new(
                7,
                kbl_core::names::Key::new("gabriel").unwrap(),
                Actor::Person,
                record,
            )
            .encode(),
        )
        .unwrap()
    };
    let raise = write(WorkRecord::FormatRaised { to: 12 });
    assert_eq!(
        raise,
        r#"{"v":12,"at_ms":7,"by":"gabriel","actor":"person","kind":"format_raised","to":12}"#
    );
    assert_eq!(
        write(WorkRecord::Taken { item: item("r1") }),
        r#"{"v":12,"at_ms":7,"by":"gabriel","actor":"person","kind":"taken","item":"r1"}"#
    );
    assert_eq!(
        write(WorkRecord::Reassigned {
            item: item("r1"),
            to: Some(kbl_core::work::Assignee::Person(
                kbl_core::names::Key::new("ana").unwrap()
            )),
        }),
        r#"{"v":12,"at_ms":7,"by":"gabriel","actor":"person","kind":"reassigned","item":"r1","to":{"person":"ana"}}"#
    );
    assert_eq!(
        write(WorkRecord::Reassigned {
            item: item("r1"),
            to: None
        }),
        r#"{"v":12,"at_ms":7,"by":"gabriel","actor":"person","kind":"reassigned","item":"r1","to":null}"#
    );
    // What a format 11 build does with a `v` above its own: the same check
    // this build makes against FORMAT, here one above it.
    let above = raise.replacen("\"v\":12", &format!("\"v\":{}", FORMAT + 1), 1);
    assert!(matches!(
        read(above.as_bytes()),
        Err(ReadError::Newer { v }) if v == FORMAT + 1
    ));
}

/// A log that raises to the thirteenth format and assigns an agent. Its
/// folded state is pinned in `wasm/golden/format_13.state.json`.
const FORMAT_13_LOG: &str = include_str!("work_format_13.jsonl");

/// Format 13 adds no record: only the raise is stamped 13.
#[test]
fn the_thirteenth_format_is_thirteen_and_adds_no_record() {
    assert_eq!(FORMAT_V13, 13);
    for (_, r) in entries(FORMAT_13_LOG) {
        assert!(r.v <= FORMAT_V12 || r.record.kind() == "format_raised");
        assert_eq!(r.record.format(), r.v, "{}", r.record.kind());
    }
}

/// Below 13 an agent assignee is refused; at 13 only the assignee claims,
/// and the tracker may drop an agent that stayed away.
#[test]
fn an_agent_assignee_routes_the_claim_once_the_log_is_at_format_13() {
    let log = entries(FORMAT_13_LOG);
    let r1 = kbl_core::names::ItemId::new("r1").unwrap();
    let holder = |n: usize| {
        fold(&log[..n]).items[&r1]
            .hold
            .as_ref()
            .map(|h| h.by.as_str().to_owned())
    };
    assert_eq!(holder(8), Some("w1".into()));
    assert_eq!(holder(12), None);
    assert_eq!(fold(&log[..13]).items[&r1].assignee, None);
    assert_eq!(holder(14), Some("w1".into()));
    let why = |offset, why| Rejection { offset, why };
    assert_eq!(
        fold(&log).rejections,
        vec![
            why(3, Reject::BadAssignee),
            why(6, Reject::NotAssignee),
            why(9, Reject::NotReady),
            why(11, Reject::NotAssignee),
        ]
    );
}

/// A log at the fourteenth format: a question and waits that end the hold
/// without using a try, and a note from an agent whose claim lapsed. Its
/// folded state is pinned in `wasm/golden/format_14.state.json`.
const FORMAT_14_LOG: &str = include_str!("work_format_14.jsonl");

/// Format 14 stamps only a release that waits.
#[test]
fn the_fourteenth_format_is_fourteen_and_stamps_only_waits() {
    assert_eq!(FORMAT_V14, 14);
    for (_, r) in entries(FORMAT_14_LOG) {
        assert_eq!(r.record.format(), r.v, "{}", r.record.kind());
    }
}

/// The question frees w1 and blocks w2 until the answer; the wait parks the
/// item until a person unflags it; w1's late note lands, w2's is refused.
#[test]
fn a_question_or_a_wait_frees_the_agent_at_format_14() {
    let log = entries(FORMAT_14_LOG);
    let mut s = kbl_core::work_fold::State::default();
    let mut refused = Vec::new();
    for (offset, r) in &log {
        if s.try_apply(*offset, r).is_err() {
            refused.push(*offset);
        }
    }
    // w2's claim on the open question, w1's claim on the parked item, w2's late note.
    assert_eq!(refused, vec![5, 9, 14]);
    let r1 = &s.items[&kbl_core::names::ItemId::new("r1").unwrap()];
    assert!(r1.hold.is_none());
    assert_eq!(r1.paths[0].tries, 0, "no failure used a try");
    assert!(r1.flagged);
    // Read below 14, the stamped releases stop the reader.
    assert!(
        log.iter()
            .any(|(_, r)| r.v == FORMAT_V14 && r.record.kind() == "released")
    );
}

/// A log at the fifteenth format: a claim that says the tracker handed the item
/// over directly. Its folded state is pinned in `wasm/golden/format_15.state.json`.
const FORMAT_15_LOG: &str = include_str!("work_format_15.jsonl");

/// Format 15 stamps only a claim that is direct; a lane claim stays format 1.
#[test]
fn the_fifteenth_format_is_fifteen_and_stamps_only_direct_claims() {
    assert_eq!(FORMAT_V15, 15);
    let log = entries(FORMAT_15_LOG);
    for (_, r) in &log {
        assert_eq!(r.record.format(), r.v, "{}", r.record.kind());
    }
    let direct = |r: &kbl_core::work::WorkRecord| {
        matches!(r, kbl_core::work::WorkRecord::Claimed { direct: true, .. })
    };
    assert_eq!(
        log.iter().filter(|(_, r)| direct(&r.record)).count(),
        1,
        "one direct claim in the log"
    );
}

/// A log below 15 refuses the stamped claim, and a reader that reads only 14 stops at it.
#[test]
fn a_direct_claim_is_refused_below_format_15_and_stops_an_older_reader() {
    let log = entries(FORMAT_15_LOG);
    let mut s = kbl_core::work_fold::State::default();
    let refused: Vec<u64> = log
        .iter()
        .filter(|(o, r)| s.try_apply(*o, r).is_err())
        .map(|(o, _)| *o)
        .collect();
    assert!(refused.is_empty(), "{refused:?}");
    let direct = log
        .iter()
        .find(|(_, r)| r.record.format() == FORMAT_V15)
        .expect("a direct claim");
    assert!(
        direct.1.v > FORMAT_V14,
        "an older reader sees v above its own"
    );
}

/// A log at the sixteenth format: a person blocks an item and lifts a block.
/// Its folded state is pinned in `wasm/golden/format_16.state.json`.
const FORMAT_16_LOG: &str = include_str!("work_format_16.jsonl");

#[test]
fn the_sixteenth_format_is_sixteen_and_stamps_both_block_records() {
    assert_eq!(FORMAT_V16, 16);
    const { assert!(FORMAT >= FORMAT_V16) };
    let log = entries(FORMAT_16_LOG);
    for (_, r) in &log {
        assert_eq!(r.record.format(), r.v, "{}", r.record.kind());
    }
    let blocks = log
        .iter()
        .filter(|(_, r)| matches!(r.record.kind(), "block_set" | "block_cleared"))
        .count();
    assert_eq!(blocks, 3);
}

/// Below 16 the fold refuses a block, and a reader that reads only 15 stops at it.
#[test]
fn a_block_is_refused_below_format_16_and_stops_an_older_reader() {
    let log = entries(FORMAT_16_LOG);
    let mut s = State::default();
    let refused: Vec<u64> = log
        .iter()
        .filter(|(o, r)| s.try_apply(*o, r).is_err())
        .map(|(o, _)| *o)
        .collect();
    assert!(refused.is_empty(), "{refused:?}");
    // The same records on a log raised only to 15 are refused.
    let mut below = State::default();
    for (o, r) in log.iter().take(4) {
        below.try_apply(*o, r).unwrap();
    }
    let raise15 = WorkLogged::new(
        1030,
        kbl_core::names::Key::new("gabriel").unwrap(),
        Actor::Person,
        WorkRecord::FormatRaised { to: 15 },
    );
    below.try_apply(4, &raise15).unwrap();
    let block = log
        .iter()
        .find(|(_, r)| r.record.kind() == "block_set")
        .unwrap();
    assert_eq!(below.try_apply(5, &block.1), Err(Reject::Format));
    assert!(
        block.1.v > FORMAT_V15,
        "an older reader sees v above its own"
    );
}

/// A log at the seventeenth format: a person adjusts triage's dependencies and
/// split. Its folded state is pinned in `wasm/golden/format_17.state.json`.
const FORMAT_17_LOG: &str = include_str!("work_format_17.jsonl");

#[test]
fn the_seventeenth_format_is_seventeen_and_stamps_only_an_edit_of_advice() {
    assert_eq!(FORMAT_V17, 17);
    const { assert!(FORMAT >= FORMAT_V17) };
    let log = entries(FORMAT_17_LOG);
    for (_, r) in &log {
        assert_eq!(r.record.format(), r.v, "{}", r.record.kind());
    }
    let edit = log
        .iter()
        .find(|(_, r)| r.record.kind() == "edited")
        .expect("an edit of advice");
    assert_eq!(edit.1.v, FORMAT_V17);
    assert_eq!(edit.1.record.format(), FORMAT_V17);
}

/// Below 17 the fold refuses the edit, and a reader that reads only 16 stops at it.
#[test]
fn an_edit_of_advice_is_refused_below_format_17_and_stops_an_older_reader() {
    let log = entries(FORMAT_17_LOG);
    let mut s = State::default();
    let refused: Vec<u64> = log
        .iter()
        .filter(|(o, r)| s.try_apply(*o, r).is_err())
        .map(|(o, _)| *o)
        .collect();
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(
        s.items[&kbl_core::names::ItemId::new("r1").unwrap()]
            .proposal
            .as_ref()
            .unwrap()
            .split,
        ["matrix"]
    );
    let mut below = State::default();
    for (o, r) in log.iter().take(2) {
        below.try_apply(*o, r).unwrap();
    }
    let raise16 = WorkLogged::new(
        1020,
        kbl_core::names::Key::new("gabriel").unwrap(),
        Actor::Person,
        WorkRecord::FormatRaised { to: 16 },
    );
    below.try_apply(2, &raise16).unwrap();
    below.try_apply(3, &log[3].1).unwrap();
    assert_eq!(below.try_apply(4, &log[4].1), Err(Reject::Format));
    assert!(
        log[4].1.v > FORMAT_V16,
        "an older reader sees v above its own"
    );
}

/// A log at the eighteenth format: a soft relation between items. Its folded
/// state is pinned in `wasm/golden/format_18.state.json`.
const FORMAT_18_LOG: &str = include_str!("work_format_18.jsonl");

#[test]
fn the_eighteenth_format_stamps_relations_and_a_triage_answer_that_names_them() {
    assert_eq!(FORMAT_V18, 18);
    for (_, r) in &entries(FORMAT_18_LOG) {
        if matches!(r.record.kind(), "related" | "unrelated") {
            assert_eq!(r.record.format(), FORMAT_V18, "{}", r.record.kind());
        }
    }
    let plain: WorkRecord =
        serde_json::from_str(r#"{"kind":"triaged","item":"r1","tier":1,"why":"w"}"#).unwrap();
    assert_eq!(
        plain.format(),
        FORMAT_V1,
        "a triage answer with no relation stays at its old format"
    );
}

/// A relation, and a triage answer that names one, are refused below 18; from
/// 18 only a person relates, never to itself, and a relation holds nothing back.
#[test]
fn relations_are_refused_below_format_18_and_from_an_agent() {
    let log = entries(FORMAT_18_LOG);
    let mut s = State::default();
    let refused: Vec<(u64, Reject)> = log
        .iter()
        .filter_map(|(o, r)| s.try_apply(*o, r).err().map(|e| (*o, e)))
        .collect();
    assert_eq!(
        refused,
        vec![
            (8, Reject::Format),
            (9, Reject::Format),
            (12, Reject::ActorNotAllowed),
            (15, Reject::Cycle),
        ]
    );
    let id = |x: &str| kbl_core::names::ItemId::new(x).unwrap();
    assert_eq!(s.related_to(&id("r1")).collect::<Vec<_>>(), [&id("r2")]);
    assert_eq!(s.related_to(&id("r2")).collect::<Vec<_>>(), [&id("r1")]);
    assert_eq!(
        s.related_to(&id("r3")).count(),
        0,
        "unrelated removes both directions"
    );
    assert_eq!(
        s.items[&id("r3")].proposal.as_ref().unwrap().related,
        [id("r1")]
    );
    assert!(s.edges.is_empty(), "a relation is not a dependency");
    assert_eq!(
        s.stage(&id("r1")),
        Some(Stage::Held),
        "r1 was claimed while related to open r2"
    );
}

/// A reader below 18 stops at the first relation.
#[test]
fn a_reader_below_18_stops_at_a_relation() {
    let first = entries(FORMAT_18_LOG)
        .into_iter()
        .find(|(_, r)| r.record.kind() == "related")
        .unwrap();
    assert!(first.1.v > 17);
    // A build that reads up to 17 refuses the record, so its fold stops there.
    let raised = entries(FORMAT_18_LOG)
        .into_iter()
        .find(|(_, r)| r.record.kind() == "format_raised")
        .unwrap();
    assert_eq!(
        raised.1.v, FORMAT_V18,
        "the raise itself stops an older reader"
    );
}

/// A log at the nineteenth format: upstream labels and milestone on an import
/// and an edit, a vocabulary, triage's class and a person's override. Its
/// folded state is pinned in `wasm/golden/format_19.state.json`.
const FORMAT_19_LOG: &str = include_str!("work_format_19.jsonl");

#[test]
fn the_nineteenth_format_is_nineteen_and_stamps_each_use_of_a_new_field() {
    assert_eq!(FORMAT_V19, 19);
    let log = entries(FORMAT_19_LOG);
    for (_, r) in &log {
        assert_eq!(r.record.format(), r.v, "{}", r.record.kind());
    }
}

#[test]
fn the_nineteenth_format_log_folds_with_its_class_and_facts() {
    let log = entries(FORMAT_19_LOG);
    let s = fold(&log);
    // The last record names a type outside the vocabulary.
    assert_eq!(s.rejections.len(), 1, "{:?}", s.rejections);
    assert_eq!(s.rejections[0].offset, 10);
    assert_eq!(s.rejections[0].why, Reject::BadClass);
    assert!(
        s.items[&kbl_core::names::ItemId::new("r2").unwrap()]
            .class
            .is_empty()
    );
    let r1 = &s.items[&kbl_core::names::ItemId::new("r1").unwrap()];
    // Reconcile replaced the labels and milestone the import carried.
    assert_eq!(r1.labels, ["Perf", "P1"]);
    assert_eq!(r1.milestone.as_deref(), Some("0.9"));
    // A person's type outlasts a later triage answer; triage's size and area replace its own.
    assert_eq!(r1.class.item_type.as_ref().unwrap().value, "bug");
    assert_eq!(r1.class.area.as_ref().unwrap().value, "dashboard");
    assert_eq!(r1.class.size.as_ref().unwrap().value, "L");
}

/// Below 19 the fold refuses each new field, and an older reader stops at the first one.
#[test]
fn a_new_field_is_refused_below_format_19() {
    let log = entries(FORMAT_19_LOG);
    for (i, (_, r)) in log.iter().enumerate().skip(1) {
        if r.v < FORMAT_V19 {
            continue;
        }
        let mut below = State::default();
        let raise = WorkLogged::new(
            1,
            kbl_core::names::Key::new("gabriel").unwrap(),
            Actor::Person,
            WorkRecord::FormatRaised { to: 18 },
        );
        below.try_apply(0, &raise).unwrap();
        // Prepare the items the record acts on, then offer the record itself.
        for (o, prior) in log[1..i].iter().filter(|(_, p)| p.v < FORMAT_V19) {
            let _ = below.try_apply(*o, prior);
        }
        assert_eq!(
            below.try_apply(i as u64, r),
            Err(Reject::Format),
            "{}",
            r.record.kind()
        );
    }
}

/// A log at the twentieth format: a vocabulary's needs, needs set by a rule,
/// triage and a person, blocks on a resource and the one record that lifts
/// them. Its folded state is pinned in `wasm/golden/format_20.state.json`.
const FORMAT_20_LOG: &str = include_str!("work_format_20.jsonl");

#[test]
fn the_twentieth_format_is_twenty_and_stamps_each_use_of_a_new_field() {
    assert_eq!(FORMAT_V20, 20);
    assert_eq!(FORMAT, FORMAT_V20);
    for (_, r) in &entries(FORMAT_20_LOG) {
        assert_eq!(r.record.format(), r.v, "{}", r.record.kind());
    }
}

#[test]
fn the_twentieth_format_log_folds_with_needs_and_a_resource_lift() {
    let s = fold(&entries(FORMAT_20_LOG));
    // The last record names a need outside the vocabulary.
    assert_eq!(s.rejections.len(), 1, "{:?}", s.rejections);
    assert_eq!(s.rejections[0].offset, 14);
    assert_eq!(s.rejections[0].why, Reject::BadClass);
    let item = |x: &str| &s.items[&kbl_core::names::ItemId::new(x).unwrap()];
    // A person's needs outlast triage's later answer.
    let n = item("r1").class.needs.as_ref().unwrap();
    assert_eq!(n.values, ["azure", "gpu"]);
    assert_eq!(n.by, kbl_core::work_fold::ClassBy::Person);
    assert!(item("r2").class.needs.is_none());
    // The lift ended both blocks on the resource and left the one with only a time.
    assert!(item("r1").block.is_none());
    let b = item("r2").block.as_ref().unwrap();
    assert!(b.resource.is_none() && b.until_ms == Some(9_000_000));
}

/// Below 20 the fold refuses each new field, and an older reader stops at the first one.
#[test]
fn a_new_field_is_refused_below_format_20() {
    let log = entries(FORMAT_20_LOG);
    for (i, (_, r)) in log.iter().enumerate().skip(1) {
        if r.v < FORMAT_V20 {
            continue;
        }
        let mut below = State::default();
        let raise = WorkLogged::new(
            1,
            kbl_core::names::Key::new("gabriel").unwrap(),
            Actor::Person,
            WorkRecord::FormatRaised { to: 19 },
        );
        below.try_apply(0, &raise).unwrap();
        for (o, prior) in log[1..i].iter().filter(|(_, p)| p.v < FORMAT_V20) {
            let _ = below.try_apply(*o, prior);
        }
        assert_eq!(
            below.try_apply(i as u64, r),
            Err(Reject::Format),
            "{}",
            r.record.kind()
        );
    }
}
