//! Stored logs must fold to the states they always did: logs from before
//! prerequisites, questions and reviews, and one that pins `context_agents`.

use kbl_core::codec::Codec;
use kbl_core::names::{ItemId, Key};
use kbl_core::work::WorkLogged;
use kbl_core::work_fold::{Reject, Stage, State};

fn log() -> Vec<(u64, WorkLogged)> {
    include_str!("work_pre_prerequisite.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect()
}

fn stage(s: &State, id: &str) -> Stage {
    s.stage(&ItemId::new(id).unwrap()).unwrap()
}

#[test]
fn an_old_log_folds_to_the_states_it_always_did() {
    let log = log();
    let mid = kbl_core::work_fold::fold(&log[..9]);
    assert_eq!(stage(&mid, "r1"), Stage::Held);
    assert_eq!(stage(&mid, "c1"), Stage::Ready);
    assert_eq!(stage(&mid, "c2"), Stage::Blocked);
    assert_eq!(stage(&mid, "r2"), Stage::Blocked);

    let end = kbl_core::work_fold::fold(&log);
    // The agent's link of c1 to r2 is refused: r2 is another tree.
    assert_eq!(end.rejections.len(), 1);
    assert_eq!(stage(&end, "r1"), Stage::Done);
    assert_eq!(stage(&end, "c1"), Stage::Done);
    assert_eq!(stage(&end, "c2"), Stage::Closed);
    assert_eq!(stage(&end, "r2"), Stage::Ready);
    // No edge in an old log waits for a release.
    assert!(end.release_edges.is_empty());
}

/// A log written before questions carried options, and before an answer could start work.
#[test]
fn an_old_question_and_answer_fold_as_they_always_did() {
    let log: Vec<_> = include_str!("work_pre_questions.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let asked = kbl_core::work_fold::fold(&log[..6]);
    assert_eq!(stage(&asked, "r1"), Stage::NeedsYou);
    assert_eq!(
        asked.items[&ItemId::new("r1").unwrap()].open_question,
        Some(5)
    );

    let end = kbl_core::work_fold::fold(&log);
    let r1 = &end.items[&ItemId::new("r1").unwrap()];
    // The answer closed the question and made no work: the item is the root and its one attempt only.
    assert_eq!(r1.open_question, None);
    assert!(r1.children.is_empty());
    assert_eq!(end.items.len(), 1);
    assert_eq!(stage(&end, "r1"), Stage::InReview);
    // Only the second answer is refused: the question was already answered.
    assert_eq!(end.rejections.len(), 1);
    assert_eq!(end.rejections[0].offset, 7);
}

#[test]
fn an_old_log_has_no_rules_and_serializes_as_before() {
    let end = kbl_core::work_fold::fold(&log());
    assert!(end.rules.is_empty());
    for id in end.items.keys() {
        assert!(end.rules_for(id).is_none());
    }
    // A state with no rules writes the same JSON a pre-rules build wrote.
    let json = serde_json::to_value(&end).unwrap();
    assert!(json.get("rules").is_none());
}

/// A log written before reviews existed: an opened pull request creates no
/// review items on its own, and no new field shows in the folded state.
#[test]
fn an_old_log_with_an_open_pull_request_makes_no_reviews() {
    let log: Vec<_> = include_str!("work_pre_review.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let end = kbl_core::work_fold::fold(&log);
    assert!(end.rejections.is_empty());
    assert_eq!(end.items.len(), 1);
    let r1 = &end.items[&ItemId::new("r1").unwrap()];
    assert!(r1.children.is_empty());
    assert!(r1.review.is_none() && r1.review_verdict.is_none() && r1.finding.is_none());
    assert!(r1.findings.is_empty());
    assert_eq!(stage(&end, "r1"), Stage::InReview);
    assert!(!end.merge_ready(&r1.id));
    // The decision has no revert check, so nothing counts as proven.
    assert!(r1.paths[0].proven.is_empty());

    // The state writes none of the review keys an older build did not know.
    let json = serde_json::to_value(&end).unwrap();
    let item = json["items"]["r1"].as_object().unwrap();
    for key in ["review", "findings", "finding", "review_verdict"] {
        assert!(!item.contains_key(key), "{key} in {item:?}");
    }
    assert!(!item["paths"][0].as_object().unwrap().contains_key("proven"));
}

/// The fold keeps which agent last wrote a context entry, and the stored
/// state carries it. `wasm/golden/agent_context.state.json` pins that state.
#[test]
fn a_context_entry_an_agent_wrote_is_kept_with_its_writer() {
    let log: Vec<_> = include_str!("work_agent_context.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let end = kbl_core::work_fold::fold(&log);
    let r1 = &end.items[&ItemId::new("r1").unwrap()];
    let key = |s: &str| Key::new(s).unwrap();
    // A person replaced `plan`, so it is no agent's. The fold lets w1 replace
    // the person's `repro` (the tracker refuses that before it is logged, and
    // a log that holds one must keep folding). w1's entries stay w1's after
    // it released the item and w2 claimed it.
    let want = [("handover", "w2"), ("notes", "w1"), ("repro", "w1")];
    assert_eq!(
        r1.context_agents,
        want.into_iter().map(|(e, a)| (key(e), key(a))).collect()
    );
    assert_eq!(r1.context[&key("repro")], 9);
    assert_eq!(r1.context[&key("plan")], 6);
    assert_eq!(r1.context[&key("notes")], 8);
    assert_eq!(r1.context[&key("handover")], 12);
    // Only the write by an agent that does not hold the item is refused.
    assert_eq!(end.rejections.len(), 1);
    assert_eq!(end.rejections[0].offset, 7);
    assert_eq!(end.rejections[0].why, Reject::NotHolder);

    let json = serde_json::to_value(&end).unwrap();
    assert_eq!(
        json["items"]["r1"]["context_agents"],
        serde_json::json!({ "handover": "w2", "notes": "w1", "repro": "w1" })
    );
    // The stored state reads back with the map in it.
    let back: State = serde_json::from_value(json).unwrap();
    assert_eq!(back, end);
}

/// A log at format 7, where a person moves items into and out of Needs you.
/// Its folded state is pinned in `wasm/golden/format_7.state.json`.
#[test]
fn a_flagged_item_waits_in_needs_you_until_a_person_moves_it_out() {
    use kbl_core::work_fold::{NeedsYou, Rejection};
    let log: Vec<_> = include_str!("work_format_7.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let r1 = ItemId::new("r1").unwrap();
    let r2 = ItemId::new("r2").unwrap();

    // Before the raise the record is refused and nothing moves.
    let before = kbl_core::work_fold::fold(&log[..6]);
    assert_eq!(stage(&before, "r1"), Stage::Ready);

    let flagged = kbl_core::work_fold::fold(&log[..8]);
    assert_eq!(flagged.needs_you(&r1), Some(NeedsYou::Flagged));
    assert_eq!(stage(&flagged, "r1"), Stage::NeedsYou);

    // A holder finishes first: the item stops once the attempt is done.
    let held = kbl_core::work_fold::fold(&log[..11]);
    assert_eq!(stage(&held, "r2"), Stage::Held);
    let done = kbl_core::work_fold::fold(&log[..12]);
    assert_eq!(done.needs_you(&r2), Some(NeedsYou::Flagged));

    let end = kbl_core::work_fold::fold(&log);
    assert_eq!(stage(&end, "r1"), Stage::Ready);
    assert!(!end.items[&r1].flagged);
    assert!(end.items[&r2].flagged);
    let why = |offset, why| Rejection { offset, why };
    assert_eq!(
        end.rejections,
        vec![
            why(5, Reject::Format),
            why(8, Reject::ActorNotAllowed),
            why(13, Reject::NoChange),
            why(14, Reject::Format),
        ]
    );
}

/// Logs from before format 7 fold with no item flagged, and their states
/// serialize without the field.
#[test]
fn an_old_log_has_no_flagged_items() {
    for log in [
        include_str!("work_pre_prerequisite.jsonl"),
        include_str!("work_format_6.jsonl"),
    ] {
        let entries: Vec<_> = log
            .lines()
            .enumerate()
            .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
            .collect();
        let s = kbl_core::work_fold::fold(&entries);
        assert!(s.items.values().all(|i| !i.flagged));
        assert!(!serde_json::to_string(&s).unwrap().contains("flagged"));
    }
}

/// A log raised to format 8: a cost reported before the raise stands, usage
/// after it is priced when it folds, an unknown model costs the floor until a
/// person prices it, and a later price leaves history alone. Its folded state
/// is pinned in `wasm/golden/format_8.state.json`.
#[test]
fn usage_is_priced_from_format_8_and_reported_costs_still_fold() {
    use kbl_core::work_fold::fold;
    let log: Vec<_> = include_str!("work_format_8.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let cost = |s: &State, id: &str| s.items[&ItemId::new(id).unwrap()].cost_micros;
    let why = |s: &State| -> Vec<(u64, Reject)> {
        s.rejections.iter().map(|r| (r.offset, r.why)).collect()
    };

    // A claim stamped 8 before the raise is refused; the legacy cost stands.
    let before = fold(&log[..5]);
    assert_eq!(why(&before), [(4, Reject::Format)]);
    assert_eq!(cost(&before, "r1"), 300_000);

    // 20k input at $3/M and 4k output at $15/M, over the floor.
    let priced = fold(&log[..10]);
    assert_eq!(cost(&priced, "r1"), 300_000 + 120_000);

    // Nothing prices `new-model` or `credits` yet: the floor is charged.
    let unpriced = fold(&log[..14]);
    assert_eq!(cost(&unpriced, "r2"), 100_000);
    assert_eq!(
        unpriced.unpriced_models().into_iter().collect::<Vec<_>>(),
        ["credits", "new-model"]
    );

    let end = fold(&log);
    // The price draws the rest: 100k tokens at $2/M.
    assert_eq!(cost(&end, "r2"), 200_000);
    assert_eq!(
        end.unpriced_models().into_iter().collect::<Vec<_>>(),
        ["credits"]
    );
    // A later price for `m` does not reprice r1.
    assert_eq!(cost(&end, "r1"), 420_000);
    assert_eq!(
        why(&end),
        [
            (4, Reject::Format),
            (15, Reject::ActorNotAllowed),
            (20, Reject::BadUsage)
        ]
    );
    let spend = end.spend();
    assert_eq!(spend.by_model["m"].cost_micros, 120_000);
    assert_eq!(spend.by_model["credits"].aic_milli, 1_500);
    assert!(spend.by_model["credits"].unpriced);
    // The stored state reads back with the prices and waiting usage in it.
    let back: State = serde_json::from_value(serde_json::to_value(&end).unwrap()).unwrap();
    assert_eq!(back, end);
}

/// A record that carries a request key folds as the same record without one,
/// and one without a key is the bytes it always was.
#[test]
fn a_request_key_changes_neither_the_fold_nor_an_unkeyed_record() {
    use kbl_core::work::RequestKey;
    let log = log();
    let plain = kbl_core::work_fold::fold(&log);
    let keyed: Vec<_> = log
        .iter()
        .map(|(o, r)| {
            let body = serde_json::json!({ "n": o });
            let req = RequestKey::new(Key::new("k1").unwrap(), "/v1/work/note", &body);
            (*o, r.clone().with_request(Some(req)))
        })
        .collect();
    let folded = kbl_core::work_fold::fold(&keyed);
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        serde_json::to_value(&folded).unwrap()
    );
    for (_, r) in &log {
        assert!(r.req.is_none());
        assert!(!String::from_utf8(r.encode()).unwrap().contains("\"req\""));
    }
    let (_, first) = &keyed[0];
    assert_eq!(&WorkLogged::decode(&first.encode()).unwrap(), first);
}

#[test]
fn a_request_digest_ignores_key_order_and_names_the_route() {
    use kbl_core::work::RequestKey;
    let k = || Key::new("k1").unwrap();
    let a = RequestKey::new(
        k(),
        "/v1/work/note",
        &serde_json::json!({ "a": 1, "b": [1, 2] }),
    );
    let b = RequestKey::new(
        k(),
        "/v1/work/note",
        &serde_json::json!({ "b": [1, 2], "a": 1 }),
    );
    assert_eq!(a, b);
    assert_ne!(
        a,
        RequestKey::new(
            k(),
            "/v1/work/tier",
            &serde_json::json!({ "a": 1, "b": [1, 2] })
        )
    );
    assert_ne!(
        a,
        RequestKey::new(
            k(),
            "/v1/work/note",
            &serde_json::json!({ "a": 2, "b": [1, 2] })
        )
    );
}

/// A log at format 10: the card actions a person takes on My queue. Its
/// folded state is pinned in `wasm/golden/format_10.state.json`.
#[test]
fn the_card_actions_fold_step_by_step() {
    use kbl_core::work_fold::{NeedsYou, Rejection};
    let log: Vec<_> = include_str!("work_format_10.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let r1 = ItemId::new("r1").unwrap();
    let r2 = ItemId::new("r2").unwrap();
    let fold = |n: usize| kbl_core::work_fold::fold(&log[..n]);

    // The attempt used its tries; a retry before the raise changes nothing.
    assert_eq!(fold(8).needs_you(&r1), Some(NeedsYou::TriesUsed));
    assert_eq!(fold(9).needs_you(&r1), Some(NeedsYou::TriesUsed));
    assert_eq!(stage(&fold(11), "r1"), Stage::Ready);

    // The question waits; a snooze hides it for ten hours, then it is back.
    assert_eq!(fold(14).queue(2000).len(), 1);
    assert!(fold(15).queue(2000).is_empty());
    assert_eq!(fold(15).queue(36_001_000).len(), 1);
    // Parked, then shown again.
    assert!(fold(17).queue(u64::MAX).is_empty());
    assert_eq!(fold(19).queue(u64::MAX).len(), 1);

    assert_eq!(stage(&fold(20), "r2"), Stage::Blocked);
    let end = fold(log.len());
    assert_eq!(stage(&end, "r2"), Stage::Ready);
    assert!(!end.is_linked(&r2, &r1));
    let why = |offset, why| Rejection { offset, why };
    assert_eq!(
        end.rejections,
        vec![
            why(8, Reject::Format),
            why(11, Reject::NoChange),
            why(15, Reject::BadTime),
            why(17, Reject::NoChange),
            why(20, Reject::ActorNotAllowed),
            why(22, Reject::NoChange),
        ]
    );
}

/// Logs from before format 10 fold with nothing snoozed or parked, and their
/// states serialize without those fields.
#[test]
fn an_old_log_has_no_snoozed_or_parked_items() {
    for log in [
        include_str!("work_pre_prerequisite.jsonl"),
        include_str!("work_format_6.jsonl"),
        include_str!("work_format_7.jsonl"),
        include_str!("work_format_8.jsonl"),
    ] {
        let entries: Vec<_> = log
            .lines()
            .enumerate()
            .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
            .collect();
        let s = kbl_core::work_fold::fold(&entries);
        assert!(
            s.items
                .values()
                .all(|i| i.snoozed.is_none() && i.parked_at_ms.is_none())
        );
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("snoozed") && !json.contains("parked_at_ms"));
    }
}

/// A log at format 11: a person's moves on a pull request and on the summary.
/// Its folded state is pinned in `wasm/golden/format_11.state.json`.
#[test]
fn the_review_actions_fold_step_by_step() {
    use kbl_core::work_fold::{Closure, Rejection};
    let log: Vec<_> = include_str!("work_format_11.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let fold = |n: usize| kbl_core::work_fold::fold(&log[..n]);
    let r1 = ItemId::new("r1").unwrap();
    let r2 = ItemId::new("r2").unwrap();
    let it = |n: usize, id: &ItemId| fold(n).items[id].clone();

    // Asked before the raise: nothing recorded. After it, the ask stands.
    assert!(it(9, &r1).review_ask.is_none());
    assert_eq!(it(11, &r1).review_ask.unwrap().reviewer, "kim");
    // A send-back answers the ask.
    let sent = it(13, &r1);
    assert!(sent.review_ask.is_none() && sent.changes_ask.is_some());
    assert_eq!(it(16, &r1).paths[0].summary_accurate, Some(true));
    // Upstream approving ends the send-back.
    assert!(it(19, &r1).changes_ask.is_none());
    // The merge closes r2, and its summary can still be marked.
    assert_eq!(it(24, &r2).closed, Some(Closure::Merged));
    let end = fold(log.len());
    assert_eq!(end.items[&r2].paths[0].summary_accurate, Some(false));
    let why = |offset, why| Rejection { offset, why };
    assert_eq!(
        end.rejections,
        vec![
            why(8, Reject::Format),
            why(11, Reject::NoChange),
            why(13, Reject::NoChange),
            why(14, Reject::ActorNotAllowed),
            why(16, Reject::NoChange),
            why(17, Reject::WrongAttempt),
            why(19, Reject::NoChange),
            why(24, Reject::Closed),
        ]
    );
}

/// Logs from before format 11 fold with no ask and no mark, and their states
/// serialize without those fields.
#[test]
fn an_old_log_has_no_review_asks_or_summary_marks() {
    for log in [
        include_str!("work_pre_prerequisite.jsonl"),
        include_str!("work_format_6.jsonl"),
        include_str!("work_format_7.jsonl"),
        include_str!("work_format_8.jsonl"),
        include_str!("work_format_10.jsonl"),
    ] {
        let entries: Vec<_> = log
            .lines()
            .enumerate()
            .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
            .collect();
        let s = kbl_core::work_fold::fold(&entries);
        let json = serde_json::to_string(&s).unwrap();
        for field in ["review_ask", "changes_ask", "summary_accurate"] {
            assert!(!json.contains(field), "{field}");
        }
    }
}

/// A log at format 12: a take, a reassign and a steer. Its folded state is
/// pinned in `wasm/golden/format_12.state.json`.
#[test]
fn the_assignee_records_fold_step_by_step() {
    use kbl_core::work::Assignee;
    use kbl_core::work_fold::{NeedsYou, Rejection};
    let log: Vec<_> = include_str!("work_format_12.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect();
    let fold = |n: usize| kbl_core::work_fold::fold(&log[..n]);
    let r1 = ItemId::new("r1").unwrap();
    let person = |k: &str| Some(Assignee::Person(Key::new(k).unwrap()));

    // Taken before the raise: nothing recorded. After it, r1 waits on gabriel.
    assert_eq!(fold(5).items[&r1].assignee, None);
    assert_eq!(fold(7).items[&r1].assignee, person("gabriel"));
    assert_eq!(fold(7).needs_you(&r1), Some(NeedsYou::Taken));
    assert_eq!(fold(10).items[&r1].assignee, person("ana"));
    // Back to nobody: Ready, and the next claim holds it.
    assert_eq!(fold(12).stage(&r1), Some(Stage::Ready));
    assert_eq!(fold(13).stage(&r1), Some(Stage::Held));
    assert_eq!(fold(14).items[&r1].paths[0].human_inputs, vec![13]);
    // Taken while held: the holder keeps it, then it stops for gabriel.
    assert_eq!(fold(17).stage(&r1), Some(Stage::Held));
    let end = fold(log.len());
    assert_eq!(end.needs_you(&r1), Some(NeedsYou::Taken));
    let why = |offset, why| Rejection { offset, why };
    assert_eq!(
        end.rejections,
        vec![
            why(4, Reject::Format),
            why(7, Reject::NoChange),
            why(8, Reject::NotReady),
            why(10, Reject::BadAssignee),
            why(14, Reject::WrongAttempt),
            why(15, Reject::ActorNotAllowed),
            why(18, Reject::NotHeld),
        ]
    );
}

/// Logs from before format 12 fold with no assignee, and their states
/// serialize without the field.
#[test]
fn an_old_log_has_no_assignee() {
    for log in [
        include_str!("work_pre_prerequisite.jsonl"),
        include_str!("work_format_10.jsonl"),
        include_str!("work_format_11.jsonl"),
    ] {
        let entries: Vec<_> = log
            .lines()
            .enumerate()
            .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
            .collect();
        let s = kbl_core::work_fold::fold(&entries);
        assert!(!serde_json::to_string(&s).unwrap().contains("assignee"));
    }
}
