//! A log written before prerequisites existed must fold to the same states.

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
    // `repro` was a person's from the start, and a person replaced `plan`.
    assert_eq!(
        r1.context_agents,
        [(key("notes"), key("w1"))].into_iter().collect()
    );
    assert_eq!(r1.context[&key("repro")], 2);
    assert_eq!(r1.context[&key("plan")], 6);
    assert_eq!(r1.context[&key("notes")], 8);
    // Only the write by an agent that does not hold the item is refused.
    assert_eq!(end.rejections.len(), 1);
    assert_eq!(end.rejections[0].offset, 7);
    assert_eq!(end.rejections[0].why, Reject::NotHolder);

    let json = serde_json::to_value(&end).unwrap();
    assert_eq!(
        json["items"]["r1"]["context_agents"],
        serde_json::json!({ "notes": "w1" })
    );
    // The stored state reads back with the map in it.
    let back: State = serde_json::from_value(json).unwrap();
    assert_eq!(back, end);
}
