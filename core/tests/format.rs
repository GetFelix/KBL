//! The record format rules of `docs/upgrades.md`: records of the first format
//! read and fold as they always did, and a reader refuses a record of a newer
//! format instead of skipping it.

use kbl_core::codec::Codec;
use kbl_core::work::{
    Actor, FORMAT, FORMAT_V1, FORMAT_V2, FORMAT_V3, FORMAT_V4, FORMAT_V5, ReadError, WorkLogged,
    WorkRecord, read,
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

/// Re-opening a prerequisite starts a new round: its release ask is dropped,
/// so the waiter's new `merged` ask frees it at the next merge.
#[test]
fn a_reopened_prerequisite_drops_its_release_ask_from_format_5() {
    let more = r#"{"at_ms":1300,"by":"gabriel","actor":"person","kind":"reopened","item":"g2"}
{"at_ms":1310,"by":"gabriel","actor":"person","kind":"accepted","item":"g2","tier":1}
{"at_ms":1320,"by":"tracker","actor":"tracker","kind":"proposed","item":"t3","parent":"r2","key":"retriage","work":"triage","title":"triage again"}
{"at_ms":1330,"by":"w2","actor":"agent","kind":"claimed","item":"t3","attempt":1,"deadline_ms":1800000090000}
{"v":3,"at_ms":1340,"by":"w2","actor":"agent","kind":"prerequisite","item":"r2","attempt":1,"prerequisite":"g2","key":"need-limits-2","done_when":"merged","cost_micros":0,"triage":"t3"}
{"at_ms":1350,"by":"w2","actor":"agent","kind":"done","item":"t3","attempt":1,"cost_micros":0}
{"at_ms":1360,"by":"gabriel","actor":"person","kind":"closed","item":"g2","why":"merged"}
"#;
    let s = format_5_with(more);
    assert_eq!(s.rejections.len(), 1, "{:?}", s.rejections);
    assert!(!s.release_edges.contains(&(id("r2"), id("g2"))));
    assert_ne!(s.stage(&id("r2")), Some(Stage::Blocked));
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
