use agents_core::content::Content;
use agents_core::names::ItemId;
use agents_core::work::{
    CloseWhy, Evidence, Phase, RefusalReason, WORK_EVENTS, WorkKind, WorkRecord,
};
use agents_core::work_fold::Stage;
use serde_json::Value;

use super::super::tests::{Fake, agent, claim, claimed_id, item, key, seed, start};
use super::{Decide, Post};
use crate::tracker::Error;

fn text(s: &str) -> Content {
    Content::Text(s.into())
}

/// The reason the call was refused, whatever kind of refusal it was.
fn why(r: Result<Value, Error>) -> String {
    match r.expect_err("expected a refusal") {
        Error::Refused { why } => why,
        Error::NotHeld(_) => "not_held".into(),
        Error::Lost(_) => "lost".into(),
        Error::Invalid(_) => "invalid".into(),
        e => panic!("unexpected {e}"),
    }
}

fn id_of(v: &Value) -> ItemId {
    item(v["item"].as_str().unwrap())
}

fn count(fake: &Fake, kind: &str) -> usize {
    fake.kinds().iter().filter(|k| *k == kind).count()
}

fn decide_args() -> Decide {
    Decide {
        phase: Phase::Cause,
        so_far: "the reader runs before the writer fills".into(),
        question: "where does it go wrong".into(),
        options: vec!["reader".into(), "writer".into()],
        chosen: "reader".into(),
        why: "the writer holds the lock".into(),
        commit: "abc123".into(),
        evidence: vec![],
    }
}

/// A tracker with `a1` ready, claimed by w1.
async fn held() -> (Fake, crate::tracker::Tracker<Fake>) {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    assert!(claimed_id(&claim(&t, "w1").await).is_some());
    (fake, t)
}

async fn post(
    t: &crate::tracker::Tracker<Fake>,
    who: &str,
    parent: &str,
    k: &str,
    kind: &str,
) -> Result<Value, Error> {
    t.post(
        &agent(who),
        &item(parent),
        key(k),
        Post::parse(kind).unwrap(),
        format!("{kind} {k}"),
    )
    .await
}

#[tokio::test]
async fn children_posted_under_a_held_item_are_live_and_block_its_completion() {
    let (fake, t) = held().await;
    let a = post(&t, "w1", "a1", "one", "split").await.unwrap();
    let b = post(&t, "w1", "a1", "two", "test").await.unwrap();
    assert_eq!(a["created"], true);
    assert_ne!(a["item"], b["item"]);
    // Both are claimable now, on the default tier.
    assert_eq!(fake.ready_count("work.ready.p2"), 3);
    let proposed: Vec<_> = fake
        .log()
        .into_iter()
        .filter_map(|l| match l.record {
            WorkRecord::Proposed { parent, work, .. } => Some((parent, work)),
            _ => None,
        })
        .collect();
    assert_eq!(
        proposed,
        vec![
            (Some(item("a1")), WorkKind::Split),
            (Some(item("a1")), WorkKind::Test)
        ]
    );
    // The parent cannot finish with open children.
    let done = t.complete(&agent("w1"), &item("a1"), 0).await;
    assert!(matches!(done, Err(Error::Refused { ref why }) if why == "open_children"));
}

#[tokio::test]
async fn the_same_key_again_answers_with_the_first_item_and_writes_nothing() {
    let (fake, t) = held().await;
    let first = post(&t, "w1", "a1", "one", "split").await.unwrap();
    let before = fake.log().len();
    let again = post(&t, "w1", "a1", "one", "split").await.unwrap();
    assert_eq!(first["item"], again["item"]);
    assert_eq!(again["created"], false);
    assert_eq!(fake.log().len(), before);
    // A restarted tracker still knows the key.
    let t2 = start(&fake).await;
    let t2 = {
        // The claim is gone with the old process; w1 gets it back as a resume.
        let _ = claim(&t2, "w1").await;
        t2
    };
    let after = post(&t2, "w1", "a1", "one", "split").await.unwrap();
    assert_eq!(first["item"], after["item"]);
    assert_eq!(count(&fake, "proposed"), 1);
    // The key names one kind of work.
    assert_eq!(
        why(post(&t2, "w1", "a1", "one", "test").await),
        "key_reused"
    );
}

#[tokio::test]
async fn an_agent_cannot_post_under_an_item_it_does_not_hold() {
    let (fake, t) = held().await;
    let before = fake.log().len();
    // w2 holds nothing; w1 holds a1.
    assert_eq!(why(post(&t, "w2", "a1", "one", "split").await), "not_held");
    assert_eq!(why(post(&t, "w2", "a1", "p", "proposal").await), "not_held");
    assert_eq!(
        why(post(&t, "w1", "nope", "one", "split").await),
        "not_held"
    );
    assert_eq!(fake.log().len(), before);
    // And after the item is handed back, even its old holder cannot.
    t.release(&agent("w1"), &item("a1"), None, 0).await.unwrap();
    assert_eq!(why(post(&t, "w1", "a1", "one", "split").await), "not_held");
    assert_eq!(fake.log().len(), before + 1);
}

#[tokio::test]
async fn an_agent_cannot_post_the_kinds_only_people_create() {
    let (fake, t) = held().await;
    let before = fake.log().len();
    for kind in ["triage", "fix", "imported", ""] {
        let r = Post::parse(kind);
        if kind == "fix" {
            assert!(r.is_ok(), "fix is a proposal");
        } else {
            assert!(matches!(r, Err(Error::Invalid(_))), "{kind:?}");
        }
    }
    let r = t
        .post(
            &agent("w1"),
            &item("a1"),
            key("k"),
            Post::Proposal,
            String::new(),
        )
        .await;
    assert_eq!(why(r), "invalid");
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn out_of_scope_work_is_a_proposal_a_person_accepts() {
    let (fake, t) = held().await;
    let p = post(&t, "w1", "a1", "found-it", "proposal").await.unwrap();
    let p_id = id_of(&p);
    // Not claimable, and a retry (or another agent's same key) does not collide.
    assert_eq!(fake.ready_count("work.ready.p2"), 1);
    assert_eq!(
        post(&t, "w1", "a1", "found-it", "proposal").await.unwrap()["created"],
        false
    );
    seed(&t, "b1", 2).await;
    claim(&t, "w2").await;
    let other = post(&t, "w2", "b1", "found-it", "proposal").await.unwrap();
    assert_ne!(id_of(&other), p_id);
    t.person_record(
        key("gab"),
        WorkRecord::Accepted {
            item: p_id.clone(),
            tier: Some(0),
        },
    )
    .await
    .unwrap();
    assert_eq!(fake.ready_count("work.ready.p0"), 1);
    let log = fake.log();
    let WorkRecord::Proposed { parent, .. } = &log
        .iter()
        .find(|l| matches!(&l.record, WorkRecord::Proposed { item, .. } if *item == p_id))
        .unwrap()
        .record
    else {
        unreachable!()
    };
    assert_eq!(*parent, None);
}

#[tokio::test]
async fn the_budget_stops_posts_and_sends_the_root_to_needs_you() {
    let (fake, t) = held().await;
    t.person_record(
        key("gab"),
        WorkRecord::BudgetSet {
            root: item("a1"),
            items_left: 1,
            max_depth: 3,
            cost_left_micros: 1_000_000,
            redos_per_item: 3,
        },
    )
    .await
    .unwrap();
    post(&t, "w1", "a1", "one", "split").await.unwrap();
    let before = count(&fake, "proposed");
    assert_eq!(
        why(post(&t, "w1", "a1", "two", "split").await),
        "over_budget"
    );
    assert_eq!(count(&fake, "proposed"), before);
    let refused: Vec<_> = fake
        .log()
        .into_iter()
        .filter_map(|l| match l.record {
            WorkRecord::Refused { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(refused, vec![RefusalReason::Budget]);
    // A second refusal does not log again; the root is flagged already.
    assert_eq!(
        why(post(&t, "w1", "a1", "three", "split").await),
        "over_budget"
    );
    assert_eq!(count(&fake, "refused"), 1);
    // A person raises the budget and the root is back.
    t.person_record(
        key("gab"),
        WorkRecord::BudgetSet {
            root: item("a1"),
            items_left: 5,
            max_depth: 3,
            cost_left_micros: 1_000_000,
            redos_per_item: 3,
        },
    )
    .await
    .unwrap();
    assert!(post(&t, "w1", "a1", "two", "split").await.is_ok());
}

#[tokio::test]
async fn a_child_too_deep_is_refused() {
    let (_fake, t) = held().await;
    // Depth 3 is allowed; a child of a depth-3 item is not.
    let mut parent = item("a1");
    for n in 0..3 {
        let c = post(&t, "w1", parent.as_str(), "d", "split").await.unwrap();
        let child = id_of(&c);
        // The agent holds only one item; the person walks it down.
        t.release(&agent("w1"), &parent, None, 0).await.unwrap();
        claim_item(&t, "w1", &child).await;
        parent = child;
        assert!(n < 3);
    }
    assert_eq!(
        why(post(&t, "w1", parent.as_str(), "d", "split").await),
        "too_deep"
    );
}

/// Claims until `w` holds `want`; other ready items are handed back.
async fn claim_item(t: &crate::tracker::Tracker<Fake>, w: &str, want: &ItemId) {
    for _ in 0..8 {
        let got = claimed_id(&claim(t, w).await);
        if got.as_deref() == Some(want.as_str()) {
            return;
        }
        if let Some(other) = got {
            t.release(&agent(w), &item(&other), None, 0).await.unwrap();
        }
    }
    panic!("never got {want}");
}

#[tokio::test]
async fn an_attempt_over_its_cap_is_refused_and_logged_once() {
    let (fake, t) = held().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    let progress = |n: u32| t.progress(&w1, &a1, text(&format!("step {n}")), None, None);
    for n in 0..100 {
        progress(n).await.unwrap();
    }
    let before = fake.log().len();
    assert_eq!(why(progress(100).await), "limit_reached");
    // One refusal on the log, then nothing more however often it loops.
    assert_eq!(fake.log().len(), before + 1);
    assert_eq!(why(progress(101).await), "limit_reached");
    assert_eq!(why(progress(102).await), "limit_reached");
    assert_eq!(fake.log().len(), before + 1);
    let last = fake.log().pop().unwrap();
    assert!(matches!(
        last.record,
        WorkRecord::Refused {
            reason: RefusalReason::Limit,
            ..
        }
    ));
    // Other kinds have room of their own.
    assert!(
        t.decide(&agent("w1"), &item("a1"), decide_args())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn links_order_siblings_refuse_cycles_and_need_a_hold() {
    let (fake, t) = held().await;
    let c1 = id_of(&post(&t, "w1", "a1", "one", "split").await.unwrap());
    let c2 = id_of(&post(&t, "w1", "a1", "two", "split").await.unwrap());
    t.link(&agent("w1"), &c2, &c1).await.unwrap();
    {
        // c2 now waits on c1.
        let core = t.0.core.lock().await;
        assert_eq!(core.state.stage(&c2), Some(Stage::Blocked));
        assert_eq!(core.state.stage(&c1), Some(Stage::Ready));
    }
    // Linking the same edge again appends nothing; the reverse closes a cycle.
    let before = fake.log().len();
    assert_eq!(why(t.link(&agent("w1"), &c2, &c1).await), "already_linked");
    assert_eq!(fake.log().len(), before);
    assert_eq!(why(t.link(&agent("w1"), &c1, &c2).await), "cycle");
    assert_eq!(why(t.link(&agent("w1"), &c1, &c1).await), "cycle");
    let kinds = fake.kinds();
    // A stranger cannot reorder someone else's items.
    assert_eq!(
        why(t.link(&agent("w2"), &c1, &item("a1")).await),
        "not_holder"
    );
    assert_eq!(fake.kinds(), kinds);
}

#[tokio::test]
async fn context_is_written_by_a_holder_and_read_back_even_after_a_restart() {
    let (fake, t) = held().await;
    t.context_add(
        &agent("w1"),
        &item("a1"),
        key("repro"),
        text("run it twice"),
    )
    .await
    .unwrap();
    // Replacing an entry keeps one entry.
    t.context_add(
        &agent("w1"),
        &item("a1"),
        key("repro"),
        text("run it three times"),
    )
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::HumanInput {
            item: item("a1"),
            attempt: None,
            input: agents_core::work::HumanKind::Note,
            text: text("it only fails on arm64"),
        },
    )
    .await
    .unwrap();
    let check = |v: &Value| {
        assert_eq!(v["context"].as_array().unwrap().len(), 1);
        assert_eq!(v["context"][0]["entry"], "repro");
        assert_eq!(v["context"][0]["text"]["text"], "run it three times");
        assert_eq!(v["notes"][0]["text"]["text"], "it only fails on arm64");
    };
    check(&t.context_read(&agent("w1"), &item("a1")).await.unwrap());
    let t2 = start(&fake).await;
    check(&t2.context_read(&agent("w1"), &item("a1")).await.unwrap());
    // A stranger may neither read nor write.
    assert_eq!(
        why(t.context_read(&agent("w2"), &item("a1")).await),
        "not_held"
    );
    assert_eq!(
        why(t
            .context_add(&agent("w2"), &item("a1"), key("x"), text("y"))
            .await),
        "not_holder"
    );
    assert_eq!(
        why(t.context_read(&agent("w1"), &item("nope")).await),
        "unknown_item"
    );
}

#[tokio::test]
async fn text_over_the_inline_limit_must_be_an_artifact() {
    let (fake, t) = held().await;
    let big = "x".repeat(agents_core::artifact::INLINE_LIMIT + 1);
    let before = fake.log().len();
    assert_eq!(
        why(t
            .context_add(&agent("w1"), &item("a1"), key("big"), text(&big))
            .await),
        "invalid"
    );
    assert_eq!(
        why(t
            .record_step(&agent("w1"), &item("a1"), "make".into(), 0, text(&big))
            .await),
        "invalid"
    );
    assert_eq!(
        why(t
            .progress(&agent("w1"), &item("a1"), text(&big), None, None)
            .await),
        "invalid"
    );
    assert_eq!(fake.log().len(), before);
    // The same bytes by hash are fine.
    let id = agents_core::artifact::ArtifactId::of(big.as_bytes());
    t.record_step(
        &agent("w1"),
        &item("a1"),
        "make".into(),
        2,
        Content::Artifact(id),
    )
    .await
    .unwrap();
    // A person's note over the limit is refused the same way.
    let r = t
        .person_record(
            key("gab"),
            WorkRecord::HumanInput {
                item: item("a1"),
                attempt: None,
                input: agents_core::work::HumanKind::Note,
                text: text(&big),
            },
        )
        .await;
    assert_eq!(why(r), "invalid");
}

#[tokio::test]
async fn a_decision_needs_phase_so_far_why_and_commit_and_cannot_cite_the_future() {
    let (fake, t) = held().await;
    let a = agent("w1");
    let a1 = item("a1");
    let before = fake.log().len();
    let blank = |f: &dyn Fn(&mut Decide)| {
        let mut d = decide_args();
        f(&mut d);
        d
    };
    for d in [
        blank(&|d| d.commit = String::new()),
        blank(&|d| d.so_far = String::new()),
    ] {
        assert_eq!(why(t.decide(&a, &a1, d).await), "missing_field");
    }
    // The why is the tool's rule, not the fold's, so old logs still fold.
    let no_why = blank(&|d| d.why = String::new());
    assert_eq!(why(t.decide(&a, &a1, no_why).await), "invalid");
    // Evidence is a stream and an offset on the log, and must come before the decision.
    let future = blank(&|d| {
        d.evidence = vec![Evidence {
            stream: WORK_EVENTS.into(),
            offset: 9999,
        }]
    });
    assert_eq!(
        why(t.decide(&a, &a1, future).await),
        "evidence_from_the_future"
    );
    assert_eq!(fake.log().len(), before);

    let ok = blank(&|d| {
        d.evidence = vec![Evidence {
            stream: WORK_EVENTS.into(),
            offset: 0,
        }]
    });
    let decided = t.decide(&a, &a1, ok).await.unwrap();
    assert_eq!(decided["attempt"], 1);
    assert_eq!(fake.log().len(), before + 1);
    // Another agent cannot decide on w1's item.
    assert_eq!(
        why(t.decide(&agent("w2"), &a1, decide_args()).await),
        "not_held"
    );
}

#[tokio::test]
async fn steps_and_edits_hang_off_the_attempt_and_an_edit_cites_a_real_decision() {
    let (fake, t) = held().await;
    let a = agent("w1");
    let a1 = item("a1");
    let step = t
        .record_step(&a, &a1, "cargo test".into(), 101, text("1 failed"))
        .await
        .unwrap();
    let d = t.decide(&a, &a1, decide_args()).await.unwrap();
    let offset = d["offset"].as_u64().unwrap();
    // An edit that cites no decision, or the step instead of a decision, is refused.
    let edit = |decision| t.edit(&a, &a1, "src/a.rs".into(), text("@@ -1 +1 @@"), decision);
    assert_eq!(why(edit(9999).await), "unknown_decision");
    assert_eq!(
        why(edit(step["offset"].as_u64().unwrap()).await),
        "unknown_decision"
    );
    edit(offset).await.unwrap();
    let state = t.0.core.lock().await.state.clone();
    let path = &state.items[&a1].paths[0];
    assert_eq!(path.steps.len(), 1);
    assert_eq!(path.decisions, vec![offset]);
    assert_eq!(path.edits.len(), 1);
    let log = fake.log();
    let WorkRecord::Step {
        exit_code, command, ..
    } = &log[step["offset"].as_u64().unwrap() as usize].record
    else {
        panic!("not a step")
    };
    assert_eq!((*exit_code, command.as_str()), (101, "cargo test"));
    assert_eq!(
        log[step["offset"].as_u64().unwrap() as usize].by.as_str(),
        "w1"
    );
}

#[tokio::test]
async fn progress_sets_the_branch_and_pr_and_a_lapsed_holder_is_refused() {
    let (fake, t) = held().await;
    let a = agent("w1");
    t.progress(
        &a,
        &item("a1"),
        text("plan: fix the reader"),
        Some("fix/reader".into()),
        Some("https://example.test/pr/1".into()),
    )
    .await
    .unwrap();
    {
        let core = t.0.core.lock().await;
        assert_eq!(
            core.state.items[&item("a1")].branch.as_deref(),
            Some("fix/reader")
        );
    }
    // The claim lapses and goes to w2: w1's next write is refused.
    fake.lapse_all();
    claim(&t, "w2").await;
    assert_eq!(
        why(t.progress(&a, &item("a1"), text("more"), None, None).await),
        "not_held"
    );
}

#[tokio::test]
async fn asking_and_answering_a_question() {
    let (_fake, t) = held().await;
    let a = agent("w1");
    let asked = t.ask(&a, &item("a1"), text("which cache?")).await.unwrap();
    let q = asked["offset"].as_u64().unwrap();
    {
        let core = t.0.core.lock().await;
        assert_eq!(core.state.items[&item("a1")].open_question, Some(q));
    }
    // A person answers the open question without naming its offset.
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text("the read cache"),
        vec![],
    )
    .await
    .unwrap();
    {
        let core = t.0.core.lock().await;
        assert_eq!(core.state.items[&item("a1")].open_question, None);
    }
    // Nothing is open now.
    let again = t
        .person_answer(key("gab"), &item("a1"), None, text("again"), vec![])
        .await;
    assert_eq!(why(again), "no_open_question");
}

#[tokio::test]
async fn a_worker_on_a_question_item_answers_for_its_parent() {
    let (_fake, t) = held().await;
    let a = agent("w1");
    let q_id = id_of(&post(&t, "w1", "a1", "q", "question").await.unwrap());
    let asked = t.ask(&a, &item("a1"), text("which cache?")).await.unwrap();
    let q = asked["offset"].as_u64().unwrap();
    claim_item(&t, "w2", &q_id).await;
    // w3 holds no question item, so it cannot answer.
    let r = t
        .answer(
            &agent("w3"),
            &item("a1"),
            q,
            text("the write cache"),
            vec![],
        )
        .await;
    assert_eq!(why(r), "not_holder");
    t.answer(&agent("w2"), &item("a1"), q, text("the read cache"), vec![])
        .await
        .unwrap();
    let core = t.0.core.lock().await;
    assert_eq!(core.state.items[&item("a1")].open_question, None);
}

#[tokio::test]
async fn people_accept_edit_reject_move_close_and_unlink() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let gab = || key("gab");
    seed(&t, "a1", 2).await;
    seed(&t, "a2", 2).await;
    // A proposal: edited, then accepted at the edited tier.
    claim(&t, "w1").await;
    let p = id_of(&post(&t, "w1", "a1", "found", "proposal").await.unwrap());
    t.person_record(
        gab(),
        WorkRecord::Edited {
            item: p.clone(),
            title: Some("the real bug".into()),
            tier: Some(0),
        },
    )
    .await
    .unwrap();
    t.person_record(
        gab(),
        WorkRecord::Accepted {
            item: p.clone(),
            tier: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(fake.ready_count("work.ready.p0"), 1);
    {
        let core = t.0.core.lock().await;
        assert_eq!(core.state.items[&p].title, "the real bug");
    }
    // A tier change republishes the item on its new tier.
    t.person_record(
        gab(),
        WorkRecord::TierChanged {
            item: p.clone(),
            tier: 3,
        },
    )
    .await
    .unwrap();
    assert_eq!(fake.ready_count("work.ready.p3"), 1);
    // A second proposal is rejected and never becomes work.
    let q = id_of(&post(&t, "w1", "a1", "noise", "proposal").await.unwrap());
    t.person_record(gab(), WorkRecord::Rejected { item: q.clone() })
        .await
        .unwrap();
    assert_eq!(t.0.core.lock().await.state.stage(&q), Some(Stage::Closed));
    // Rejecting an accepted item is refused.
    let r = t
        .person_record(gab(), WorkRecord::Rejected { item: p.clone() })
        .await;
    assert_eq!(why(r), "not_a_proposal");
    // Unlink frees a blocked item.
    let c = id_of(&post(&t, "w1", "a1", "c", "split").await.unwrap());
    let d = id_of(&post(&t, "w1", "a1", "d", "split").await.unwrap());
    t.link(&agent("w1"), &c, &d).await.unwrap();
    assert_eq!(t.0.core.lock().await.state.stage(&c), Some(Stage::Blocked));
    t.person_record(
        gab(),
        WorkRecord::Unlinked {
            item: c.clone(),
            to: d.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(t.0.core.lock().await.state.stage(&c), Some(Stage::Ready));
    // Closing the parent cancels the child.
    t.person_record(
        gab(),
        WorkRecord::Closed {
            item: item("a1"),
            why: CloseWhy::Dismissed,
        },
    )
    .await
    .unwrap();
    assert_eq!(t.0.core.lock().await.state.stage(&c), Some(Stage::Closed));
    // An agent may not use the person routes' records.
    let bad = t
        .person_record(
            gab(),
            WorkRecord::TierChanged {
                item: item("a2"),
                tier: 7,
            },
        )
        .await;
    assert_eq!(why(bad), "bad_tier");
}

#[tokio::test]
async fn the_whole_flow_leaves_only_records_the_fold_accepts() {
    let (fake, t) = held().await;
    let a = agent("w1");
    post(&t, "w1", "a1", "one", "split").await.unwrap();
    t.decide(&a, &item("a1"), decide_args()).await.unwrap();
    let _ = post(&t, "w2", "a1", "no", "split").await;
    let _ = t
        .decide(
            &a,
            &item("a1"),
            Decide {
                commit: String::new(),
                ..decide_args()
            },
        )
        .await;
    // Replaying the log from scratch gives no rejections: nothing refused was written.
    let entries: Vec<_> = fake
        .log()
        .into_iter()
        .enumerate()
        .map(|(i, l)| (i as u64, l))
        .collect();
    let replay = agents_core::work_fold::fold(&entries);
    assert!(replay.rejections.is_empty(), "{:?}", replay.rejections);
}

#[tokio::test]
async fn a_child_is_read_by_the_holder_of_its_parent_and_not_by_a_stranger() {
    let (_fake, t) = held().await;
    let c = id_of(&post(&t, "w1", "a1", "one", "split").await.unwrap());
    assert!(t.context_read(&agent("w1"), &c).await.is_ok());
    assert_eq!(why(t.context_read(&agent("w2"), &c).await), "not_held");
}

#[tokio::test]
async fn the_question_text_is_read_back_with_the_context() {
    let (_fake, t) = held().await;
    let asked = t
        .ask(&agent("w1"), &item("a1"), text("which cache?"))
        .await
        .unwrap();
    let q = asked["offset"].as_u64().unwrap();
    let core = t.0.core.lock().await;
    assert_eq!(core.texts[&q], text("which cache?"));
}

#[tokio::test]
async fn an_answer_to_a_stale_question_is_refused() {
    let (fake, t) = held().await;
    let a = agent("w1");
    let q_id = id_of(&post(&t, "w1", "a1", "q", "question").await.unwrap());
    let old = t.ask(&a, &item("a1"), text("one?")).await.unwrap()["offset"]
        .as_u64()
        .unwrap();
    let open = t.ask(&a, &item("a1"), text("two?")).await.unwrap()["offset"]
        .as_u64()
        .unwrap();
    claim_item(&t, "w2", &q_id).await;
    let before = fake.log().len();
    let r = t
        .answer(&agent("w2"), &item("a1"), old, text("late"), vec![])
        .await;
    assert_eq!(why(r), "not_open_question");
    assert_eq!(fake.log().len(), before);
    let r = t
        .person_answer(key("gab"), &item("a1"), Some(old), text("late"), vec![])
        .await;
    assert_eq!(why(r), "not_open_question");
    t.answer(&agent("w2"), &item("a1"), open, text("now"), vec![])
        .await
        .unwrap();
}

#[tokio::test]
async fn evidence_is_bounded_in_size_and_count() {
    let (fake, t) = held().await;
    let ev = |stream: &str| Evidence {
        stream: stream.into(),
        offset: 0,
    };
    let big = "s".repeat(super::MAX_FIELD + 1);
    let before = fake.log().len();
    let mut d = decide_args();
    d.evidence = vec![ev(&big)];
    assert_eq!(why(t.decide(&agent("w1"), &item("a1"), d).await), "invalid");
    let mut d = decide_args();
    d.evidence = (0..=super::MAX_EVIDENCE).map(|_| ev("logs")).collect();
    assert_eq!(why(t.decide(&agent("w1"), &item("a1"), d).await), "invalid");
    let mut d = decide_args();
    d.why = String::new();
    assert_eq!(why(t.decide(&agent("w1"), &item("a1"), d).await), "invalid");
    let q = t
        .ask(&agent("w1"), &item("a1"), text("which?"))
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    let before = before + 1;
    let r = t
        .person_answer(key("gab"), &item("a1"), Some(q), text("x"), vec![ev(&big)])
        .await;
    assert_eq!(why(r), "invalid");
    let many = (0..=super::MAX_EVIDENCE).map(|_| ev("logs")).collect();
    let r = t
        .person_answer(key("gab"), &item("a1"), Some(q), text("x"), many)
        .await;
    assert_eq!(why(r), "invalid");
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn an_edited_title_is_bounded_and_not_empty() {
    let (fake, t) = held().await;
    let before = fake.log().len();
    let edit = |title: String| WorkRecord::Edited {
        item: item("a1"),
        title: Some(title),
        tier: None,
    };
    let r = t.person_record(key("gab"), edit(String::new())).await;
    assert_eq!(why(r), "invalid");
    let r = t
        .person_record(key("gab"), edit("t".repeat(super::MAX_FIELD + 1)))
        .await;
    assert_eq!(why(r), "invalid");
    assert_eq!(fake.log().len(), before);
}
