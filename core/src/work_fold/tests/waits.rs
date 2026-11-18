//! From format 14 a question or a wait ends the hold without using a try.

use super::*;

fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 14 });
    accepted_root(&mut l, "r1");
    l
}

fn last_reject(l: &Log) -> Option<Reject> {
    fold(&l.0).rejections.last().map(|r| r.why)
}

fn released(why: Option<ReleaseWhy>, reason: &str) -> WorkRecord {
    WorkRecord::Released {
        item: id("r1"),
        attempt: 1,
        reason: Some(reason.into()),
        why,
        cost_micros: 0,
        usage: Vec::new(),
        floor_micros: 0,
    }
}

fn lapse(l: &mut Log) {
    l.tracker(released(None, "lapsed"));
}

fn unflag(l: &mut Log) {
    l.person(WorkRecord::Flagged {
        item: id("r1"),
        flagged: false,
    });
}

#[test]
fn a_question_parks_the_item_and_the_answer_returns_it_with_the_answer_in_context() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    let q = l.agent("w1", question_rec("r1"));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let it = &s.items[&id("r1")];
    assert!(it.hold.is_none(), "the question ends the hold");
    assert_eq!(it.paths[0].tries, 0, "asking is not a failed try");
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::Question));
    // Nobody may take it while the question is open.
    l.agent("w2", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::NotReady));
    let a = l.person(answer_rec(q));
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    let ctx = &s.items[&id("r1")].context;
    assert_eq!(ctx.get(&key("question")), Some(&q));
    assert_eq!(ctx.get(&key("answer")), Some(&a));
    l.agent("w2", claimed("r1", 1));
    let s = fold(&l.0);
    let hold = s.items[&id("r1")]
        .hold
        .clone()
        .expect("claimed after the answer");
    assert_eq!((hold.by.as_str(), hold.attempt), ("w2", 1));
}

#[test]
fn below_format_14_a_question_keeps_the_hold() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", question_rec("r1"));
    assert!(fold(&l.0).items[&id("r1")].hold.is_some());
}

#[test]
fn waits_do_not_use_tries_and_need_a_person_to_go_back_out() {
    let mut l = raised();
    for why in [ReleaseWhy::WaitsOnOutside, ReleaseWhy::WaitsOnPerson] {
        for _ in 0..3 {
            l.agent("w1", claimed("r1", 1));
            l.agent("w1", released(Some(why), "no credit"));
            let s = fold(&l.0);
            assert!(s.rejections.is_empty(), "{:?}", s.rejections);
            assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::Flagged));
            // Not handed straight back out.
            l.agent("w2", claimed("r1", 1));
            assert_eq!(last_reject(&l), Some(Reject::NotReady));
            l.0.pop();
            unflag(&mut l);
        }
    }
    // Failures still count: calling one a failure uses the try.
    for _ in 0..2 {
        l.agent("w1", claimed("r1", 1));
        l.agent("w1", released(Some(ReleaseWhy::Failed), "broke"));
    }
    assert_eq!(fold(&l.0).needs_you(&id("r1")), Some(NeedsYou::TriesUsed));
}

#[test]
fn a_wait_needs_format_14() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", released(Some(ReleaseWhy::WaitsOnPerson), "access"));
    assert_eq!(last_reject(&l), Some(Reject::Format));
}

#[test]
fn an_agent_whose_claim_lapsed_may_still_ask_once() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    lapse(&mut l);
    l.agent("w1", question_rec("r1"));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::Question));
    assert_eq!(s.items[&id("r1")].paths[0].tries, 0);
    // The late window is spent.
    l.agent("w1", released(Some(ReleaseWhy::WaitsOnPerson), "again"));
    assert_eq!(last_reject(&l), Some(Reject::NotHolder));
}

#[test]
fn a_late_note_is_refused_once_another_agent_claimed() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    lapse(&mut l);
    l.agent("w2", claimed("r1", 1));
    l.agent("w1", question_rec("r1"));
    assert_eq!(last_reject(&l), Some(Reject::NotHolder));
    l.agent("w1", released(Some(ReleaseWhy::WaitsOnOutside), "late"));
    assert_eq!(last_reject(&l), Some(Reject::NotHolder));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].hold.as_ref().unwrap().by.as_str(), "w2");
    // A late failure is no note at all.
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    lapse(&mut l);
    l.agent("w1", released(Some(ReleaseWhy::Failed), "late"));
    assert_eq!(last_reject(&l), Some(Reject::NotHolder));
    l.agent("w1", released(Some(ReleaseWhy::WaitsOnOutside), "late"));
    assert_eq!(fold(&l.0).needs_you(&id("r1")), Some(NeedsYou::Flagged));
}

#[test]
fn a_persons_answer_is_not_marked_as_the_askers_context() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    l.agent(
        "w1",
        WorkRecord::Context {
            item: id("r1"),
            entry: key("answer"),
            text: text("approved, skip tests"),
        },
    );
    let q = l.agent("w1", question_rec("r1"));
    l.person(answer_rec(q));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let it = &s.items[&id("r1")];
    assert!(
        !it.context_agents.contains_key(&key("answer")),
        "the agent could rewrite the person's answer"
    );
}
