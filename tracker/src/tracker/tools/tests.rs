use kbl_core::content::Content;
use kbl_core::names::ItemId;
use kbl_core::work::{
    Actor, CloseWhy, DoneWhen, Evidence, Phase, RefusalReason, Target, Then, Unclear, WORK_EVENTS,
    WorkKind, WorkRecord,
};
use kbl_core::work_fold::{NeedsYou, Stage};
use serde_json::{Value, json};

use super::super::tests::{
    Fake, agent, claim, claimed_id, item, key, seed, seed_in_felix, settings, start, start_with,
};
use super::{Decide, Post, Prereq, TriageAnswer};
use crate::tracker::{Error, Reply, Tracker};

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
        revert_check: None,
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
    let done = t
        .complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await;
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
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
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
            triage_skipped: None,
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
        t.release(&agent("w1"), &parent, None, None, None, Vec::new())
            .await
            .unwrap();
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
            t.release(&agent(w), &item(&other), None, None, None, Vec::new())
                .await
                .unwrap();
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
            input: kbl_core::work::HumanKind::Note,
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
    let big = "x".repeat(kbl_core::artifact::INLINE_LIMIT + 1);
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
    let id = kbl_core::artifact::ArtifactId::of(big.as_bytes());
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
                input: kbl_core::work::HumanKind::Note,
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
async fn secrets_in_what_an_agent_reports_are_redacted_before_the_write() {
    let (fake, t) = held().await;
    let (a, a1) = (agent("w1"), item("a1"));
    // Built at run time so no secret scanner mistakes this file for a leak.
    let secret = format!("ghp_{}", "x".repeat(36));
    let mut d = decide_args();
    d.so_far = format!("the token {secret} is in the env");
    let decided = t.decide(&a, &a1, d).await.unwrap();
    let WorkRecord::Decision { so_far, .. } = fake.log().pop().unwrap().record else {
        panic!("not a decision")
    };
    assert_eq!(so_far, "the token [redacted] is in the env");
    t.record_step(&a, &a1, "env".into(), 0, text(&format!("GH={secret}")))
        .await
        .unwrap();
    let WorkRecord::Step { output, .. } = fake.log().pop().unwrap().record else {
        panic!("not a step")
    };
    assert_eq!(output, text("GH=[redacted]"));
    let at = decided["offset"].as_u64().unwrap();
    t.edit(&a, &a1, "x.rs".into(), text(&format!("+{secret}")), at)
        .await
        .unwrap();
    let WorkRecord::Edit { hunk, .. } = fake.log().pop().unwrap().record else {
        panic!("not an edit")
    };
    assert_eq!(hunk, text("+[redacted]"));
}

#[tokio::test]
async fn a_token_in_a_persons_answer_is_cut_before_the_write() {
    let (fake, t) = held().await;
    let (a, a1) = (agent("w1"), item("a1"));
    let q = t
        .ask(&a, &a1, None, text("which key?"), vec![], None, Vec::new())
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    // Built at run time so no secret scanner mistakes this file for a leak.
    let secret = format!("ghp_{}", "y".repeat(36));
    t.person_answer(
        key("gab"),
        &a1,
        Some(q),
        text(&format!("use {secret}")),
        vec![],
        Reply::default(),
    )
    .await
    .unwrap();
    let WorkRecord::Answer { text: said, .. } = fake.log().pop().unwrap().record else {
        panic!("not an answer")
    };
    assert_eq!(said, text("use [redacted]"));
}

#[tokio::test]
async fn evidence_must_cite_a_record_of_the_same_tree() {
    let (fake, t) = held().await;
    let a = agent("w1");
    let a1 = item("a1");
    // a2 is another tree: its import cannot back a decision on a1.
    seed(&t, "a2", 3).await;
    let a2_import = fake
        .log()
        .iter()
        .rposition(|l| l.record.kind() == "imported")
        .unwrap() as u64;
    let cite = |stream: &str, offset: u64| {
        let mut d = decide_args();
        d.evidence = vec![Evidence {
            stream: stream.into(),
            offset,
        }];
        d
    };
    let before = fake.log().len();
    assert_eq!(
        why(t.decide(&a, &a1, cite(WORK_EVENTS, a2_import)).await),
        "evidence_outside_tree"
    );
    assert_eq!(
        why(t.decide(&a, &a1, cite("agents.log.r-1", 0)).await),
        "evidence_stream"
    );
    // A person's answer is held to the same rule.
    let q = t
        .ask(&a, &a1, None, text("which?"), vec![], None, Vec::new())
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    let foreign = vec![Evidence {
        stream: WORK_EVENTS.into(),
        offset: a2_import,
    }];
    let r = t
        .person_answer(
            key("gab"),
            &a1,
            Some(q),
            text("x"),
            foreign,
            Reply::default(),
        )
        .await;
    assert_eq!(why(r), "evidence_outside_tree");
    assert_eq!(fake.log().len(), before + 1);
    // A record of a1's own tree, here its question, is fine.
    let own = vec![Evidence {
        stream: WORK_EVENTS.into(),
        offset: q,
    }];
    t.person_answer(key("gab"), &a1, Some(q), text("x"), own, Reply::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_writer_the_fold_refuses_learns_nothing_about_the_evidence() {
    let (fake, t) = held().await;
    seed(&t, "a2", 3).await;
    let a2_import = fake
        .log()
        .iter()
        .rposition(|l| l.record.kind() == "imported")
        .unwrap() as u64;
    let a1 = item("a1");
    let q = t
        .ask(
            &agent("w1"),
            &a1,
            None,
            text("which?"),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    let foreign = vec![Evidence {
        stream: WORK_EVENTS.into(),
        offset: a2_import,
    }];
    // w2 holds no question under a1, so the fold's refusal is what it sees.
    let r = t
        .answer(&agent("w2"), &a1, q, text("x"), foreign, None)
        .await;
    assert_eq!(why(r), "not_holder");
}

#[tokio::test]
async fn a_restarted_tracker_still_holds_evidence_to_the_tree() {
    let (fake, t) = held().await;
    seed(&t, "a2", 3).await;
    let a2_import = fake
        .log()
        .iter()
        .rposition(|l| l.record.kind() == "imported")
        .unwrap() as u64;
    let a1_import = fake
        .log()
        .iter()
        .position(|l| l.record.kind() == "imported")
        .unwrap() as u64;
    drop(t);
    let t2 = start(&fake).await;
    // The claim is gone with the old process; w1 gets it back as a resume.
    let _ = claim(&t2, "w1").await;
    let cite = |offset: u64| {
        let mut d = decide_args();
        d.evidence = vec![Evidence {
            stream: WORK_EVENTS.into(),
            offset,
        }];
        d
    };
    let (a, a1) = (agent("w1"), item("a1"));
    assert_eq!(
        why(t2.decide(&a, &a1, cite(a2_import)).await),
        "evidence_outside_tree"
    );
    t2.decide(&a, &a1, cite(a1_import)).await.unwrap();
}

#[tokio::test]
async fn a_new_prerequisite_cites_evidence_of_its_own_tree() {
    let (fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    import(&t, "a2", "GetFelix/felix#2").await;
    let a2_import = fake
        .log()
        .iter()
        .rposition(|l| l.record.kind() == "imported")
        .unwrap() as u64;
    let a1_import = fake
        .log()
        .iter()
        .position(|l| l.record.kind() == "imported")
        .unwrap() as u64;
    let citing = |offset: u64| match gateway() {
        Prereq::New {
            target, title, why, ..
        } => Prereq::New {
            target,
            title,
            why,
            evidence: vec![Evidence {
                stream: WORK_EVENTS.into(),
                offset,
            }],
        },
        other => other,
    };
    assert_eq!(
        why(t
            .prerequisite(
                &w1,
                &a1,
                None,
                key("cas"),
                citing(a2_import),
                DoneWhen::Merged,
                Vec::new()
            )
            .await),
        "evidence_outside_tree"
    );
    t.prerequisite(
        &w1,
        &a1,
        None,
        key("cas"),
        citing(a1_import),
        DoneWhen::Merged,
        Vec::new(),
    )
    .await
    .unwrap();
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
async fn asking_is_charged_at_least_the_floor() {
    let fake = Fake::default();
    let floor = crate::tracker::Settings {
        cost_floor_micros: 500,
        review_limit: 0,
        ..settings()
    };
    let t = start_with(&fake, floor).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.ask(
        &agent("w1"),
        &item("a1"),
        None,
        text("which?"),
        vec![],
        None,
        Vec::new(),
    )
    .await
    .unwrap();
    let log = fake.log();
    let Some(WorkRecord::Question { cost_micros, .. }) = log.last().map(|l| &l.record) else {
        panic!("the last record is not a question");
    };
    assert_eq!(*cost_micros, 500);
}

#[tokio::test]
async fn a_prerequisite_is_charged_at_least_the_floor() {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github("GetFelix/felix-gateway")])].into();
    let floor = crate::tracker::Settings {
        grants,
        cost_floor_micros: 500,
        review_limit: 0,
        ..settings()
    };
    let t = start_with(&fake, floor).await;
    import(&t, "a1", "GetFelix/felix#1").await;
    person(
        &t,
        WorkRecord::Accepted {
            item: item("a1"),
            tier: Some(2),
            triage_skipped: None,
        },
    )
    .await;
    claim(&t, "w1").await;
    t.prerequisite(
        &agent("w1"),
        &item("a1"),
        None,
        key("cas"),
        gateway(),
        DoneWhen::Merged,
        Vec::new(),
    )
    .await
    .unwrap();
    let log = fake.log();
    let Some(WorkRecord::Prerequisite { cost_micros, .. }) = log.last().map(|l| &l.record) else {
        panic!("the last record is not a prerequisite");
    };
    assert_eq!(*cost_micros, 500);
}

#[tokio::test]
async fn asking_and_answering_a_question() {
    let (_fake, t) = held().await;
    let a = agent("w1");
    let asked = t
        .ask(
            &a,
            &item("a1"),
            None,
            text("which cache?"),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap();
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
        Reply::default(),
    )
    .await
    .unwrap();
    {
        let core = t.0.core.lock().await;
        assert_eq!(core.state.items[&item("a1")].open_question, None);
    }
    // Nothing is open now.
    let again = t
        .person_answer(
            key("gab"),
            &item("a1"),
            None,
            text("again"),
            vec![],
            Reply::default(),
        )
        .await;
    assert_eq!(why(again), "no_open_question");
}

#[tokio::test]
async fn a_worker_on_a_question_item_answers_for_its_parent() {
    let (_fake, t) = held().await;
    let a = agent("w1");
    let q_id = id_of(&post(&t, "w1", "a1", "q", "question").await.unwrap());
    let asked = t
        .ask(
            &a,
            &item("a1"),
            None,
            text("which cache?"),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap();
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
            None,
        )
        .await;
    assert_eq!(why(r), "not_holder");
    t.answer(
        &agent("w2"),
        &item("a1"),
        q,
        text("the read cache"),
        vec![],
        None,
    )
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
            labels: None,
            milestone: None,
            item_type: None,
            area: None,
            size: None,
            needs: None,
            depends_on: None,
            split: None,
            proposal: None,
        },
    )
    .await
    .unwrap();
    t.person_record(
        gab(),
        WorkRecord::Accepted {
            item: p.clone(),
            tier: None,
            triage_skipped: None,
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
    let replay = kbl_core::work_fold::fold(&entries);
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
        .ask(
            &agent("w1"),
            &item("a1"),
            None,
            text("which cache?"),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let q = asked["offset"].as_u64().unwrap();
    let core = t.0.core.lock().await;
    assert_eq!(
        core.recall.texts.peek(q).unwrap().clone(),
        text("which cache?")
    );
}

#[tokio::test]
async fn an_answer_to_a_stale_question_is_refused() {
    let (fake, t) = held().await;
    let a = agent("w1");
    let q_id = id_of(&post(&t, "w1", "a1", "q", "question").await.unwrap());
    let old = t
        .ask(
            &a,
            &item("a1"),
            None,
            text("one?"),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    let open = t
        .ask(
            &a,
            &item("a1"),
            None,
            text("two?"),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    claim_item(&t, "w2", &q_id).await;
    let before = fake.log().len();
    let r = t
        .answer(&agent("w2"), &item("a1"), old, text("late"), vec![], None)
        .await;
    assert_eq!(why(r), "not_open_question");
    assert_eq!(fake.log().len(), before);
    let r = t
        .person_answer(
            key("gab"),
            &item("a1"),
            Some(old),
            text("late"),
            vec![],
            Reply::default(),
        )
        .await;
    assert_eq!(why(r), "not_open_question");
    t.answer(&agent("w2"), &item("a1"), open, text("now"), vec![], None)
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
        .ask(
            &agent("w1"),
            &item("a1"),
            None,
            text("which?"),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    let before = before + 1;
    let r = t
        .person_answer(
            key("gab"),
            &item("a1"),
            Some(q),
            text("x"),
            vec![ev(&big)],
            Reply::default(),
        )
        .await;
    assert_eq!(why(r), "invalid");
    let many = (0..=super::MAX_EVIDENCE).map(|_| ev("logs")).collect();
    let r = t
        .person_answer(
            key("gab"),
            &item("a1"),
            Some(q),
            text("x"),
            many,
            Reply::default(),
        )
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
        labels: None,
        milestone: None,
        item_type: None,
        area: None,
        size: None,
        needs: None,
        depends_on: None,
        split: None,
        proposal: None,
    };
    let r = t.person_record(key("gab"), edit(String::new())).await;
    assert_eq!(why(r), "invalid");
    let r = t
        .person_record(key("gab"), edit("t".repeat(super::MAX_FIELD + 1)))
        .await;
    assert_eq!(why(r), "invalid");
    assert_eq!(fake.log().len(), before);
}

fn github(repo: &str) -> Target {
    Target {
        source: "github".into(),
        repo: repo.into(),
    }
}

/// A tracker where w1 holds `a1`, an issue in `GetFelix/felix`, and may reach `GetFelix/felix-gateway`.
async fn granted() -> (Fake, crate::tracker::Tracker<Fake>) {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github("GetFelix/felix-gateway")])].into();
    let t = start_with(
        &fake,
        crate::tracker::Settings {
            grants,
            ..settings()
        },
    )
    .await;
    import(&t, "a1", "GetFelix/felix#1").await;
    person(
        &t,
        WorkRecord::Accepted {
            item: item("a1"),
            tier: Some(2),
            triage_skipped: None,
        },
    )
    .await;
    assert!(claimed_id(&claim(&t, "w1").await).is_some());
    (fake, t)
}

async fn person(t: &crate::tracker::Tracker<Fake>, record: WorkRecord) {
    t.person_record(key("gab"), record).await.unwrap();
}

async fn import(t: &crate::tracker::Tracker<Fake>, id: &str, upstream: &str) {
    person(
        t,
        WorkRecord::Imported {
            item: item(id),
            source: "github".into(),
            upstream_id: upstream.into(),
            title: format!("fix {id}"),
            labels: vec![],
            milestone: None,
        },
    )
    .await;
}

fn gateway() -> Prereq {
    Prereq::New {
        target: Target {
            source: "github".into(),
            repo: "GetFelix/felix-gateway".into(),
        },
        title: "gateway needs a CAS call".into(),
        why: text("the client cannot swap atomically"),
        evidence: vec![],
    }
}

async fn stage(t: &crate::tracker::Tracker<Fake>, id: &ItemId) -> Option<Stage> {
    t.0.core.lock().await.state.stage(id)
}

#[tokio::test]
async fn a_prerequisite_blocks_the_item_without_using_an_attempt() {
    let (fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    let reply = t
        .prerequisite(
            &w1,
            &a1,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(reply["status"], "blocked");
    let g = id_of(&json!({ "item": reply["prerequisite"] }));
    assert_eq!(stage(&t, &a1).await, Some(Stage::Blocked));
    // The proposal sits in Triage of the other repo, with the reason beside it.
    assert_eq!(stage(&t, &g).await, Some(Stage::Proposed));
    {
        let core = t.0.core.lock().await;
        let it = &core.state.items[&g];
        assert_eq!(it.target.as_ref().unwrap().repo, "GetFelix/felix-gateway");
        let at = it.context.values().next().unwrap();
        assert_eq!(
            core.recall.texts.peek(*at).unwrap().clone(),
            text("the client cannot swap atomically")
        );
        // No try was spent and nothing was released or given up.
        assert_eq!(core.state.items[&a1].paths[0].tries, 0);
    }
    assert!(
        !fake
            .kinds()
            .iter()
            .any(|k| k == "released" || k == "gave_up")
    );
    // Nobody else can claim a blocked item.
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    // A retry answers with the first call's work and writes nothing.
    let before = fake.log().len();
    let again = t
        .prerequisite(
            &w1,
            &a1,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(again["created"], false);
    assert_eq!(again["prerequisite"], reply["prerequisite"]);
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn the_item_is_ready_again_when_the_prerequisite_is_merged() {
    let (_fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    let reply = t
        .prerequisite(
            &w1,
            &a1,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await
        .unwrap();
    let g = id_of(&json!({ "item": reply["prerequisite"] }));
    person(
        &t,
        WorkRecord::Accepted {
            item: g.clone(),
            tier: None,
            triage_skipped: None,
        },
    )
    .await;
    assert_eq!(stage(&t, &a1).await, Some(Stage::Blocked));
    person(
        &t,
        WorkRecord::Progress {
            item: g.clone(),
            attempt: 1,
            note: text("pr"),
            branch: None,
            pr: Some("GetFelix/felix-gateway#9".into()),
        },
    )
    .await;
    person(
        &t,
        WorkRecord::Closed {
            item: g.clone(),
            why: CloseWhy::Merged,
        },
    )
    .await;
    assert_eq!(stage(&t, &a1).await, Some(Stage::Ready));
    // The next claim resumes the same attempt and sees where the work landed.
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
    let ctx = t.context_read(&agent("w2"), &a1).await.unwrap();
    assert_eq!(ctx["prerequisites"][0]["item"], g.as_str());
    assert_eq!(ctx["prerequisites"][0]["pr"], "GetFelix/felix-gateway#9");
}

#[tokio::test]
async fn a_release_link_waits_for_the_release_not_the_merge() {
    let (_fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    let reply = t
        .prerequisite(
            &w1,
            &a1,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Released,
            Vec::new(),
        )
        .await
        .unwrap();
    let g = id_of(&json!({ "item": reply["prerequisite"] }));
    person(
        &t,
        WorkRecord::Accepted {
            item: g.clone(),
            tier: None,
            triage_skipped: None,
        },
    )
    .await;
    person(
        &t,
        WorkRecord::Closed {
            item: g.clone(),
            why: CloseWhy::Merged,
        },
    )
    .await;
    assert_eq!(stage(&t, &a1).await, Some(Stage::Blocked));
    person(
        &t,
        WorkRecord::Shipped {
            item: g,
            release: "0.4.0".into(),
        },
    )
    .await;
    assert_eq!(stage(&t, &a1).await, Some(Stage::Ready));
}

#[tokio::test]
async fn an_agent_reaches_only_the_repos_its_grants_name() {
    let (fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    import(&t, "f1", "other/secret#4").await;
    import(&t, "f2", "GetFelix/felix-gateway#4").await;
    let kinds = fake.kinds();
    // A new prerequisite in a repo nobody granted.
    let elsewhere = Prereq::New {
        target: Target {
            source: "github".into(),
            repo: "other/secret".into(),
        },
        title: "x".into(),
        why: text("y"),
        evidence: vec![],
    };
    assert_eq!(
        why(t
            .prerequisite(
                &w1,
                &a1,
                None,
                key("k"),
                elsewhere,
                DoneWhen::Merged,
                Vec::new()
            )
            .await),
        "no_grant"
    );
    // An existing item in one, which looks the same as a missing item.
    for foreign in ["f1", "nope"] {
        let r = t
            .prerequisite(
                &w1,
                &a1,
                None,
                key("k"),
                Prereq::Existing(item(foreign)),
                DoneWhen::Merged,
                Vec::new(),
            )
            .await;
        assert_eq!(why(r), "unknown_item");
    }
    // A granted repo name in another source is not granted.
    let other_source = Prereq::New {
        target: Target {
            source: "jira".into(),
            repo: "GetFelix/felix-gateway".into(),
        },
        title: "x".into(),
        why: text("y"),
        evidence: vec![],
    };
    assert_eq!(
        why(t
            .prerequisite(
                &w1,
                &a1,
                None,
                key("k"),
                other_source,
                DoneWhen::Merged,
                Vec::new()
            )
            .await),
        "no_grant"
    );
    assert_eq!(fake.kinds(), kinds);
    person(
        &t,
        WorkRecord::Imported {
            item: item("f3"),
            source: "jira".into(),
            upstream_id: "GetFelix/felix-gateway#4".into(),
            title: "fix f3".into(),
            labels: vec![],
            milestone: None,
        },
    )
    .await;
    let kinds = fake.kinds();
    let r = t
        .prerequisite(
            &w1,
            &a1,
            None,
            key("k"),
            Prereq::Existing(item("f3")),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await;
    assert_eq!(why(r), "unknown_item");
    // The plain link tool never crosses trees, granted or not.
    assert_eq!(why(t.link(&w1, &a1, &item("f2")).await), "cross_root");
    assert_eq!(fake.kinds(), kinds);
    // An item in a granted repo may be linked instead of creating a duplicate.
    let reply = t
        .prerequisite(
            &w1,
            &a1,
            None,
            key("k"),
            Prereq::Existing(item("f2")),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(reply["created"], false);
    assert_eq!(stage(&t, &a1).await, Some(Stage::Blocked));
    // And an agent that holds nothing cannot name a prerequisite at all.
    assert_eq!(
        why(t
            .prerequisite(
                &agent("w2"),
                &a1,
                None,
                key("k"),
                gateway(),
                DoneWhen::Merged,
                Vec::new()
            )
            .await),
        "not_held"
    );
}

#[tokio::test]
async fn prerequisites_share_the_per_attempt_post_cap() {
    let (_fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    t.person_record(
        key("gab"),
        WorkRecord::BudgetSet {
            root: a1.clone(),
            items_left: 100,
            max_depth: 3,
            cost_left_micros: 1_000_000,
            redos_per_item: 3,
        },
    )
    .await
    .unwrap();
    for n in 0..24 {
        post(&t, "w1", "a1", &format!("p{n}"), "test")
            .await
            .unwrap();
    }
    assert_eq!(
        why(t
            .prerequisite(
                &w1,
                &a1,
                None,
                key("cas"),
                gateway(),
                DoneWhen::Merged,
                Vec::new()
            )
            .await),
        "limit_reached"
    );
    assert_eq!(stage(&t, &a1).await, Some(Stage::Held));
}

#[tokio::test]
async fn a_release_name_is_not_empty_and_is_bounded() {
    let (_fake, t) = granted().await;
    for release in [String::new(), "x".repeat(5000)] {
        let r = t
            .person_record(
                key("gab"),
                WorkRecord::Shipped {
                    item: item("a1"),
                    release,
                },
            )
            .await;
        assert_eq!(why(r), "invalid");
    }
}

/// w1 holds a1, decides, then asks which cache with two options.
async fn asked_with_options() -> (Fake, crate::tracker::Tracker<Fake>, u64) {
    let (fake, t) = held().await;
    t.decide(&agent("w1"), &item("a1"), decide_args())
        .await
        .unwrap();
    let q = t
        .ask(
            &agent("w1"),
            &item("a1"),
            None,
            text("which cache?"),
            vec!["read".into(), "write".into()],
            Some("read".into()),
            Vec::new(),
        )
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    (fake, t, q)
}

async fn resume_ids(t: &crate::tracker::Tracker<Fake>) -> Vec<ItemId> {
    let core = t.0.core.lock().await;
    core.state
        .items
        .values()
        .filter(|i| i.work == WorkKind::Resume)
        .map(|i| i.id.clone())
        .collect()
}

fn chosen(c: &str) -> Reply {
    Reply {
        chosen: Some(c.into()),
        ..Reply::default()
    }
}

#[tokio::test]
async fn one_key_answers_with_a_choice_and_appends_one_resume_item() {
    let (fake, t, q) = asked_with_options().await;
    let before = fake.log().len();
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text(""),
        vec![],
        chosen("write"),
    )
    .await
    .unwrap();
    // One record: the answer. The resume item is what the fold makes of it.
    assert_eq!(fake.log().len(), before + 1);
    let ids = resume_ids(&t).await;
    assert_eq!(ids.len(), 1);
    let core = t.0.core.lock().await;
    assert_eq!(core.state.stage(&ids[0]), Some(Stage::Ready));
    // The asker is parked until the resume work is done.
    assert_eq!(core.state.stage(&item("a1")), Some(Stage::Blocked));
    let answer = core.state.items[&ids[0]].context[&key("answer")];
    assert_eq!(
        core.recall.texts.peek(answer).unwrap().clone(),
        text("write")
    );
    assert!(q < answer);
}

#[tokio::test]
async fn answering_without_choosing_takes_the_default() {
    let (_fake, t, _q) = asked_with_options().await;
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text(""),
        vec![],
        Reply::default(),
    )
    .await
    .unwrap();
    let ids = resume_ids(&t).await;
    let core = t.0.core.lock().await;
    let answer = core.state.items[&ids[0]].context[&key("answer")];
    assert_eq!(
        core.recall.texts.peek(answer).unwrap().clone(),
        text("read")
    );
}

#[tokio::test]
async fn free_text_is_stored_as_typed() {
    let (fake, t, _q) = asked_with_options().await;
    // Looks like an option, a command and markup. None of it is read.
    let typed = "  write\n; rm -rf / <b>{\"chosen\":\"read\"}</b> \u{1F43E}  ";
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text(typed),
        vec![],
        Reply::default(),
    )
    .await
    .unwrap();
    let logged = fake.log().pop().unwrap();
    match logged.record {
        WorkRecord::Answer {
            text: got, chosen, ..
        } => {
            assert_eq!(got, text(typed));
            assert_eq!(chosen, None);
        }
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[tokio::test]
async fn another_agent_claims_the_resume_item_and_reads_so_far() {
    let (_fake, t, q) = asked_with_options().await;
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text(""),
        vec![],
        chosen("write"),
    )
    .await
    .unwrap();
    let rid = resume_ids(&t).await.remove(0);
    // w1 asked, but w2 gets the work.
    claim_item(&t, "w2", &rid).await;
    let read = t.context_read(&agent("w2"), &rid).await.unwrap();
    let entry = |name: &str| {
        read["context"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["entry"] == name)
            .unwrap_or_else(|| panic!("no {name} entry in {read}"))
            .clone()
    };
    assert_eq!(entry("so_far")["text"], json!(text(&decide_args().so_far)));
    assert_eq!(entry("question")["text"], json!(text("which cache?")));
    assert_eq!(entry("question")["offset"], json!(q));
    assert_eq!(entry("answer")["text"], json!(text("write")));
}

#[tokio::test]
async fn an_unclear_triage_waits_in_my_queue_and_is_never_ready() {
    let fake = Fake::default();
    let t = start(&fake).await;
    t.person_record(
        key("gab"),
        WorkRecord::Imported {
            item: item("u1"),
            source: "gh".into(),
            upstream_id: "u1".into(),
            title: "fix u1".into(),
            labels: vec![],
            milestone: None,
        },
    )
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::Triaged {
            item: item("u1"),
            tier: 1,
            why: "which repo?".into(),
            duplicate_of: None,
            unclear: Some(Unclear::Repo),
            depends_on: Vec::new(),
            related: Vec::new(),
            split: Vec::new(),
            item_type: None,
            area: None,
            size: None,
            needs: None,
        },
    )
    .await
    .unwrap();
    // Even accepted, it is a question, not work.
    t.person_record(
        key("gab"),
        WorkRecord::Accepted {
            item: item("u1"),
            tier: Some(1),
            triage_skipped: None,
        },
    )
    .await
    .unwrap();
    {
        let core = t.0.core.lock().await;
        assert_eq!(core.state.stage(&item("u1")), Some(Stage::NeedsYou));
        assert_eq!(core.state.needs_you(&item("u1")), Some(NeedsYou::Unclear));
    }
    assert_eq!(fake.ready_count("work.ready.p1"), 0);
    assert!(claimed_id(&claim(&t, "w1").await).is_none());
    // The person's answer reaches an agent as resume work.
    t.person_answer(
        key("gab"),
        &item("u1"),
        None,
        text("the gateway"),
        vec![],
        Reply::default(),
    )
    .await
    .unwrap();
    assert_eq!(resume_ids(&t).await.len(), 1);
}

#[tokio::test]
async fn an_answer_that_names_another_repo_creates_a_prerequisite_there() {
    let (_fake, t, _q) = asked_with_options().await;
    let moves = Reply {
        moves_to: Some((
            Target {
                source: "github".into(),
                repo: "GetFelix/felix-gateway".into(),
            },
            "gateway needs a CAS call".into(),
        )),
        ..Reply::default()
    };
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text("belongs in felix-gateway"),
        vec![],
        moves,
    )
    .await
    .unwrap();
    assert!(resume_ids(&t).await.is_empty());
    let core = t.0.core.lock().await;
    let (_, p) = core.state.edges.iter().next().expect("a1 waits on it");
    let p = &core.state.items[p];
    assert_eq!(p.target.as_ref().unwrap().repo, "GetFelix/felix-gateway");
    assert_eq!(core.state.stage(&p.id), Some(Stage::Proposed));
    assert_eq!(core.state.stage(&item("a1")), Some(Stage::Blocked));
}

#[tokio::test]
async fn a_second_answer_to_the_same_question_appends_nothing() {
    let (fake, t, q) = asked_with_options().await;
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text(""),
        vec![],
        chosen("read"),
    )
    .await
    .unwrap();
    let before = fake.log().len();
    let again = t
        .person_answer(
            key("gab"),
            &item("a1"),
            None,
            text(""),
            vec![],
            chosen("write"),
        )
        .await;
    assert_eq!(why(again), "no_open_question");
    // Naming the answered question gets the same refusal from the fold.
    let named = t
        .person_answer(
            key("gab"),
            &item("a1"),
            Some(q),
            text(""),
            vec![],
            chosen("write"),
        )
        .await;
    assert_eq!(why(named), "not_open_question");
    assert_eq!(fake.log().len(), before);
    assert_eq!(resume_ids(&t).await.len(), 1);
}

#[tokio::test]
async fn an_option_the_question_did_not_offer_is_refused() {
    let (fake, t, _q) = asked_with_options().await;
    let before = fake.log().len();
    let r = t
        .person_answer(
            key("gab"),
            &item("a1"),
            None,
            text(""),
            vec![],
            chosen("both"),
        )
        .await;
    assert_eq!(why(r), "bad_option");
    // An option and free text together are not an answer either.
    let r = t
        .person_answer(
            key("gab"),
            &item("a1"),
            None,
            text("both"),
            vec![],
            chosen("read"),
        )
        .await;
    assert_eq!(why(r), "invalid");
    assert_eq!(fake.log().len(), before);
    assert!(resume_ids(&t).await.is_empty());
}

#[tokio::test]
async fn an_agent_cannot_move_work_to_another_repo_through_an_answer() {
    let (fake, t, q) = asked_with_options().await;
    let q_id = id_of(&post(&t, "w1", "a1", "q", "question").await.unwrap());
    claim_item(&t, "w2", &q_id).await;
    let before = fake.log().len();
    // The agent route has no way to ask for it; the fold refuses it if a record says so.
    let mut core = t.0.core.lock().await;
    let forged = WorkRecord::Answer {
        item: item("a1"),
        question: q,
        text: text("to the gateway"),
        evidence: vec![],
        chosen: None,
        then: Some(Then::Prerequisite {
            target: Target {
                source: "github".into(),
                repo: "GetFelix/felix-gateway".into(),
            },
            title: "steer".into(),
        }),
    };
    let r = t.append(&mut core, Actor::Agent, key("w2"), forged).await;
    assert_eq!(why(r.map(|_| json!(null))), "actor_not_allowed");
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn options_are_bounded_when_asking() {
    let (fake, t) = held().await;
    let before = fake.log().len();
    let (w1, a1) = (agent("w1"), item("a1"));
    let ask = |options: Vec<String>, default: Option<String>| {
        t.ask(&w1, &a1, None, text("which?"), options, default, Vec::new())
    };
    let many = (0..=kbl_core::work::MAX_ASK_OPTIONS)
        .map(|i| format!("o{i}"))
        .collect();
    assert_eq!(why(ask(many, None).await), "invalid");
    let long = "x".repeat(kbl_core::work::MAX_ASK_OPTION_LEN + 1);
    assert_eq!(why(ask(vec![long], None).await), "invalid");
    assert_eq!(
        why(ask(vec!["a".into(), "a".into()], None).await),
        "invalid"
    );
    assert_eq!(
        why(ask(vec!["a".into()], Some("b".into())).await),
        "invalid"
    );
    assert_eq!(fake.log().len(), before);
}

/// A root with the triage item ingest posts for it, claimed by `who`.
async fn held_triage(t: &Tracker<Fake>, who: &str) {
    seed(t, "a1", 2).await;
    t.person_record(
        key("gab"),
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: WorkKind::Triage,
            title: "Triage: fix a1".into(),
            review: None,
        },
    )
    .await
    .unwrap();
    claim_item(t, who, &item("a1-t")).await;
}

#[tokio::test]
async fn an_agent_answers_the_triage_item_it_holds() {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github("GetFelix/felix")])].into();
    let t = start_with(
        &fake,
        crate::tracker::Settings {
            grants,
            ..settings()
        },
    )
    .await;
    held_triage(&t, "w1").await;
    seed_in_felix(&t, "a2").await;
    let reply = t
        .triage(
            &agent("w1"),
            &item("a1-t"),
            ans(1, "blocks the release".into(), Some(item("a2")), None),
        )
        .await
        .unwrap();
    assert_eq!(reply["triaged"], "a1");
    let last = fake.log().pop().unwrap();
    assert_eq!(last.actor, Actor::Agent);
    assert_eq!(
        last.record,
        WorkRecord::Triaged {
            item: item("a1"),
            tier: 1,
            why: "blocks the release".into(),
            duplicate_of: Some(item("a2")),
            unclear: None,
            depends_on: Vec::new(),
            related: Vec::new(),
            split: Vec::new(),
            item_type: None,
            area: None,
            size: None,
            needs: None,
        }
    );
    // A retry of the same answer writes nothing.
    let again = t
        .triage(
            &agent("w1"),
            &item("a1-t"),
            ans(1, "blocks the release".into(), Some(item("a2")), None),
        )
        .await
        .unwrap();
    assert_eq!(again, reply);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "triaged").count(), 1);
    // It is a proposal: the tier a person accepted stands.
    {
        let core = t.0.core.lock().await;
        let root = &core.state.items[&item("a1")];
        assert_eq!(root.tier, Some(2));
        assert_eq!(root.proposal.as_ref().map(|p| p.tier), Some(1));
    }
    // The triage item can now be finished.
    t.complete(&agent("w1"), &item("a1-t"), None, Vec::new())
        .await
        .unwrap();
    assert_eq!(
        t.0.core.lock().await.state.stage(&item("a1-t")),
        Some(Stage::Done)
    );
}

#[tokio::test]
async fn an_unclear_triage_from_an_agent_goes_to_a_person() {
    let fake = Fake::default();
    let t = start(&fake).await;
    held_triage(&t, "w1").await;
    t.triage(
        &agent("w1"),
        &item("a1-t"),
        ans(2, "which repo?".into(), None, Some(Unclear::Repo)),
    )
    .await
    .unwrap();
    let core = t.0.core.lock().await;
    assert_eq!(core.state.needs_you(&item("a1")), Some(NeedsYou::Unclear));
}

#[tokio::test]
async fn triage_is_refused_without_the_triage_item_or_with_a_bad_answer() {
    let fake = Fake::default();
    let t = start(&fake).await;
    held_triage(&t, "w1").await;
    let records = fake.log().len();
    let triage =
        |who: &'static str, id: &'static str, tier: u8, why: String, dup: Option<&'static str>| {
            let t = t.clone();
            async move {
                t.triage(&agent(who), &item(id), ans(tier, why, dup.map(item), None))
                    .await
            }
        };
    // Another agent, and the holder naming the root instead of the triage item.
    assert_eq!(
        why(triage("w2", "a1-t", 1, "w".into(), None).await),
        "not_held"
    );
    assert_eq!(
        why(triage("w1", "a1", 1, "w".into(), None).await),
        "not_held"
    );
    // A duplicate that is no item, or the item itself.
    assert_eq!(
        why(triage("w1", "a1-t", 1, "w".into(), Some("nope")).await),
        "invalid"
    );
    assert_eq!(
        why(triage("w1", "a1-t", 1, "w".into(), Some("a1")).await),
        "invalid"
    );
    assert_eq!(
        why(triage("w1", "a1-t", 1, "x".repeat(5000), None).await),
        "invalid"
    );
    assert_eq!(fake.log().len(), records);
    assert_eq!(
        why(triage("w1", "a1-t", 9, "w".into(), None).await),
        "bad_tier"
    );
    // Holding work of another kind does not let an agent triage.
    seed(&t, "b1", 2).await;
    let other = claimed_id(&claim(&t, "w2").await).unwrap();
    let got = t
        .triage(&agent("w2"), &item(&other), ans(1, "w".into(), None, None))
        .await;
    assert_eq!(why(got), "invalid");
}

#[tokio::test]
async fn triage_cannot_name_a_duplicate_the_agent_may_not_see() {
    let (fake, t) = granted().await;
    let w1 = agent("w1");
    // w1 holds a1-t, a triage item in GetFelix/felix; other/secret is not granted.
    person(
        &t,
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: WorkKind::Triage,
            title: "Triage: fix a1".into(),
            review: None,
        },
    )
    .await;
    claim_item(&t, "w1", &item("a1-t")).await;
    import(&t, "f1", "other/secret#4").await;
    let before = fake.log().len();
    let mut refusals = Vec::new();
    for dup in ["f1", "nope"] {
        let r = t
            .triage(
                &w1,
                &item("a1-t"),
                ans(1, "w".into(), Some(item(dup)), None),
            )
            .await;
        refusals.push(format!("{:?}", r.unwrap_err()).replace(dup, "X"));
    }
    assert_eq!(refusals[0], refusals[1]);
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn a_new_holders_identical_answer_is_recorded_under_its_own_hold() {
    let fake = Fake::default();
    let t = start(&fake).await;
    held_triage(&t, "w1").await;
    let answer = |who: &'static str| {
        let t = t.clone();
        async move {
            t.triage(
                &agent(who),
                &item("a1-t"),
                ans(1, "same".into(), None, None),
            )
            .await
            .unwrap()
        }
    };
    answer("w1").await;
    t.release(&agent("w1"), &item("a1-t"), None, None, None, Vec::new())
        .await
        .unwrap();
    claim_item(&t, "w2", &item("a1-t")).await;
    let second = answer("w2").await;
    assert_eq!(fake.kinds().iter().filter(|k| *k == "triaged").count(), 2);
    let core = t.0.core.lock().await;
    assert_eq!(
        core.state.items[&item("a1")]
            .proposal
            .as_ref()
            .map(|p| p.at),
        second["offset"].as_u64()
    );
}

#[tokio::test]
async fn triage_after_the_hold_is_gone_is_not_held() {
    let fake = Fake::default();
    let t = start(&fake).await;
    held_triage(&t, "w1").await;
    t.release(&agent("w1"), &item("a1-t"), None, None, None, Vec::new())
        .await
        .unwrap();
    let before = fake.log().len();
    let r = t
        .triage(&agent("w1"), &item("a1-t"), ans(1, "w".into(), None, None))
        .await;
    assert_eq!(why(r), "not_held");
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn a_triage_holder_that_varies_its_answer_is_stopped_and_refused_once() {
    let fake = Fake::default();
    let t = start(&fake).await;
    held_triage(&t, "w1").await;
    let answer = |n: usize| {
        let t = t.clone();
        async move {
            t.triage(
                &agent("w1"),
                &item("a1-t"),
                ans(1, format!("why {n}"), None, None),
            )
            .await
        }
    };
    for n in 0..4 {
        answer(n).await.unwrap();
    }
    let before = fake.log().len();
    assert_eq!(why(answer(4).await), "limit_reached");
    assert_eq!(why(answer(5).await), "limit_reached");
    // One refusal on the log for the attempt, and no answer past the cap.
    assert_eq!(fake.log().len(), before + 1);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "triaged").count(), 4);
}

#[tokio::test]
async fn proposals_waiting_for_a_person_are_capped_per_root_and_a_decision_frees_a_slot() {
    let (fake, t) = granted().await;
    let mut first = String::new();
    for n in 0..8 {
        let r = post(&t, "w1", "a1", &format!("p{n}"), "proposal")
            .await
            .unwrap();
        if n == 0 {
            first = r["item"].as_str().unwrap().to_owned();
        }
    }
    let before = fake.log().len();
    assert_eq!(
        why(post(&t, "w1", "a1", "over", "proposal").await),
        "limit_reached"
    );
    assert_eq!(
        why(t
            .prerequisite(
                &agent("w1"),
                &item("a1"),
                None,
                key("cas"),
                gateway(),
                DoneWhen::Merged,
                Vec::new()
            )
            .await),
        "limit_reached"
    );
    // One refusal on the log for the attempt, and the same key still answers.
    assert_eq!(fake.log().len(), before + 1);
    assert_eq!(
        post(&t, "w1", "a1", "p0", "proposal").await.unwrap()["created"],
        false
    );
    t.person_record(key("gab"), WorkRecord::Rejected { item: item(&first) })
        .await
        .unwrap();
    post(&t, "w1", "a1", "again", "proposal").await.unwrap();
}

#[tokio::test]
async fn a_triage_answer_can_carry_dependencies_and_a_split() {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github("GetFelix/felix")])].into();
    let t = start_with(
        &fake,
        crate::tracker::Settings {
            grants,
            ..settings()
        },
    )
    .await;
    held_triage(&t, "w1").await;
    seed_in_felix(&t, "a2").await;
    let w1 = agent("w1");
    let mut answer = ans(1, "needs a2 first".into(), None, None);
    answer.depends_on = vec![item("a2")];
    answer.split = vec!["reader".into(), "writer".into()];
    let reply = t.triage(&w1, &item("a1-t"), answer.clone()).await.unwrap();
    let last = fake.log().pop().unwrap();
    assert_eq!(
        last.record,
        WorkRecord::Triaged {
            item: item("a1"),
            tier: 1,
            why: "needs a2 first".into(),
            duplicate_of: None,
            unclear: None,
            depends_on: vec![item("a2")],
            related: Vec::new(),
            split: vec!["reader".into(), "writer".into()],
            item_type: None,
            area: None,
            size: None,
            needs: None,
        }
    );
    // The same answer again is a retry; changing only the split is a new answer.
    assert_eq!(
        t.triage(&w1, &item("a1-t"), answer.clone()).await.unwrap(),
        reply
    );
    assert_eq!(fake.kinds().iter().filter(|k| *k == "triaged").count(), 1);
    let mut changed = answer;
    changed.split = vec!["whole".into()];
    t.triage(&w1, &item("a1-t"), changed).await.unwrap();
    assert_eq!(fake.kinds().iter().filter(|k| *k == "triaged").count(), 2);
    // It is advice: no link and no child exists.
    let core = t.0.core.lock().await;
    assert!(core.state.edges.is_empty());
    assert!(!core.state.items.values().any(|i| i.title == "whole"));
}

#[tokio::test]
async fn a_triage_answer_can_name_related_items_from_format_18_and_blocks_nothing() {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github("GetFelix/felix")])].into();
    let t = start_with(
        &fake,
        crate::tracker::Settings {
            grants,
            ..settings()
        },
    )
    .await;
    held_triage(&t, "w1").await;
    seed_in_felix(&t, "a2").await;
    let w1 = agent("w1");
    let mut answer = ans(1, "same boxes as a2".into(), None, None);
    answer.related = vec![item("a2")];
    let before = fake.log().len();
    let err = t
        .triage(&w1, &item("a1-t"), answer.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Refused { ref why } if why == "needs_format_18"),
        "{err:?}"
    );
    assert_eq!(fake.log().len(), before, "nothing is written below 18");
    // Without a relation the old format still takes the answer.
    t.triage(&w1, &item("a1-t"), ans(1, "plain".into(), None, None))
        .await
        .unwrap();

    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 18 })
        .await
        .unwrap();
    t.triage(&w1, &item("a1-t"), answer.clone()).await.unwrap();
    let last = fake.log().pop().unwrap();
    assert_eq!((last.v, last.record.kind()), (18, "triaged"));
    let reply = t.triage(&w1, &item("a1-t"), answer).await.unwrap();
    assert_eq!(reply["triaged"], "a1");
    assert_eq!(
        fake.kinds().iter().filter(|k| *k == "triaged").count(),
        2,
        "a retry is not rewritten"
    );
    // Advice only: no relation and no dependency until a person writes one.
    let core = t.0.core.lock().await;
    assert!(core.state.edges.is_empty() && core.state.related.is_empty());
}

#[tokio::test]
async fn triage_refuses_bad_related_items_and_writes_nothing() {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github("GetFelix/felix")])].into();
    let t = start_with(
        &fake,
        crate::tracker::Settings {
            grants,
            ..settings()
        },
    )
    .await;
    held_triage(&t, "w1").await;
    seed_in_felix(&t, "a2").await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 18 })
        .await
        .unwrap();
    let before = fake.log().len();
    let w1 = agent("w1");
    let with = |related: Vec<&str>, depends_on: Vec<&str>| {
        let mut a = ans(1, "w".into(), None, None);
        a.related = related.into_iter().map(item).collect();
        a.depends_on = depends_on.into_iter().map(item).collect();
        a
    };
    for a in [
        with(vec!["nope"], vec![]),
        with(vec!["a1"], vec![]),
        with(vec!["a1-t"], vec![]),
        with(vec!["a2", "a2"], vec![]),
        with(vec!["a2"], vec!["a2"]),
    ] {
        assert!(t.triage(&w1, &item("a1-t"), a).await.is_err());
    }
    // Only the holder of the triage item may answer.
    let w2 = agent("w2");
    assert!(
        t.triage(&w2, &item("a1-t"), with(vec!["a2"], vec![]))
            .await
            .is_err()
    );
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn triage_refuses_bad_dependencies_and_splits_and_writes_nothing() {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github("GetFelix/felix")])].into();
    let t = start_with(
        &fake,
        crate::tracker::Settings {
            grants,
            ..settings()
        },
    )
    .await;
    held_triage(&t, "w1").await;
    seed_in_felix(&t, "a2").await;
    let before = fake.log().len();
    let try_with = |depends_on: Vec<&'static str>, split: Vec<String>| {
        let t = t.clone();
        async move {
            let mut a = ans(1, "w".into(), None, None);
            a.depends_on = depends_on.into_iter().map(item).collect();
            a.split = split;
            t.triage(&agent("w1"), &item("a1-t"), a).await
        }
    };
    // Unknown, the item itself, its own triage item, a repeat.
    for deps in [vec!["nope"], vec!["a1"], vec!["a1-t"], vec!["a2", "a2"]] {
        assert_eq!(
            why(try_with(deps.clone(), vec![]).await),
            "invalid",
            "{deps:?}"
        );
    }
    let many: Vec<String> = (0..9).map(|n| format!("part {n}")).collect();
    assert_eq!(why(try_with(vec![], many.clone()).await), "invalid");
    assert_eq!(why(try_with(vec![], vec![String::new()]).await), "invalid");
    assert_eq!(
        why(try_with(vec![], vec!["x".repeat(300)]).await),
        "invalid"
    );
    assert_eq!(fake.log().len(), before);
    // A person's record is held to the same bounds.
    let err = t
        .person_record(
            key("gab"),
            WorkRecord::Triaged {
                item: item("a1"),
                tier: 1,
                why: "w".into(),
                duplicate_of: None,
                unclear: None,
                depends_on: vec![],
                related: Vec::new(),
                split: many,
                item_type: None,
                area: None,
                size: None,
                needs: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn a_person_adjusts_triage_s_advice_only_at_format_17_and_on_the_answer_they_read() {
    let fake = Fake::default();
    let t = start(&fake).await;
    // a1 is imported and not accepted: triage's answer is a proposal for a person.
    t.person_record(
        key("gab"),
        WorkRecord::Imported {
            item: item("a1"),
            source: "gh".into(),
            upstream_id: "a1".into(),
            title: "fix a1".into(),
            labels: vec![],
            milestone: None,
        },
    )
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: WorkKind::Triage,
            title: "Triage: fix a1".into(),
            review: None,
        },
    )
    .await
    .unwrap();
    claim_item(&t, "w1", &item("a1-t")).await;
    seed(&t, "a2", 2).await;
    t.triage(&agent("w1"), &item("a1-t"), ans(1, "w".into(), None, None))
        .await
        .unwrap();
    let at = {
        let core = t.0.core.lock().await;
        core.state.items[&item("a1")].proposal.as_ref().unwrap().at
    };
    let edit = |proposal: u64, deps: Vec<&str>, split: Vec<String>| WorkRecord::Edited {
        item: item("a1"),
        title: None,
        tier: None,
        depends_on: Some(deps.into_iter().map(item).collect()),
        split: Some(split),
        proposal: Some(proposal),
        area: None,
        item_type: None,
        labels: None,
        milestone: None,
        size: None,
        needs: None,
    };
    let before = fake.log().len();
    // The log is below 17: refused, nothing written.
    let r = t
        .person_record(key("gab"), edit(at, vec!["a2"], vec!["x".into()]))
        .await;
    assert_eq!(why(r), "format");
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 17 })
        .await
        .unwrap();
    let before = before + 1;
    // Bounds are triage's own; a stale offset and an unknown item are refused.
    let many: Vec<String> = (0..9).map(|n| format!("part {n}")).collect();
    let r = t.person_record(key("gab"), edit(at, vec![], many)).await;
    assert_eq!(why(r), "invalid");
    let r = t
        .person_record(key("gab"), edit(at + 1, vec![], vec!["x".into()]))
        .await;
    assert_ne!(why(r), "ok");
    let r = t
        .person_record(key("gab"), edit(at, vec!["nope"], vec![]))
        .await;
    assert_ne!(why(r), "ok");
    assert_eq!(fake.log().len(), before);
    t.person_record(key("gab"), edit(at, vec!["a2"], vec!["x".into()]))
        .await
        .unwrap();
    let core = t.0.core.lock().await;
    let p = core.state.items[&item("a1")].proposal.clone().unwrap();
    assert_eq!(
        (p.depends_on, p.split),
        (vec![item("a2")], vec!["x".to_owned()])
    );
    drop(core);
    // The agent repeating its own answer after the person's edit is a retry, so the edit stands.
    let triaged = fake.kinds().iter().filter(|k| *k == "triaged").count();
    t.triage(&agent("w1"), &item("a1-t"), ans(1, "w".into(), None, None))
        .await
        .unwrap();
    assert_eq!(
        fake.kinds().iter().filter(|k| *k == "triaged").count(),
        triaged
    );
}

#[tokio::test]
async fn triage_cannot_name_a_dependency_the_agent_may_not_see() {
    let (fake, t) = granted().await;
    person(
        &t,
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: WorkKind::Triage,
            title: "Triage: fix a1".into(),
            review: None,
        },
    )
    .await;
    claim_item(&t, "w1", &item("a1-t")).await;
    import(&t, "f1", "other/secret#4").await;
    let before = fake.log().len();
    let mut refusals = Vec::new();
    for dep in ["f1", "nope"] {
        let mut a = ans(1, "w".into(), None, None);
        a.depends_on = vec![item(dep)];
        let r = t.triage(&agent("w1"), &item("a1-t"), a).await;
        refusals.push(format!("{:?}", r.unwrap_err()).replace(dep, "X"));
    }
    assert_eq!(refusals[0], refusals[1]);
    assert_eq!(fake.log().len(), before);
}

fn ans(
    tier: u8,
    why: String,
    duplicate_of: Option<ItemId>,
    unclear: Option<Unclear>,
) -> TriageAnswer {
    TriageAnswer {
        tier,
        why,
        duplicate_of,
        unclear,
        depends_on: Vec::new(),
        related: Vec::new(),
        split: Vec::new(),
        item_type: None,
        area: None,
        size: None,
        needs: None,
    }
}

#[tokio::test]
async fn a_prerequisite_proposal_cites_the_record_that_filed_it() {
    let (fake, t) = granted().await;
    let reply = t
        .prerequisite(
            &agent("w1"),
            &item("a1"),
            None,
            key("cas"),
            gateway(),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await
        .unwrap();
    let g = id_of(&json!({ "item": reply["prerequisite"] }));
    let origin = fake
        .log()
        .iter()
        .rposition(|l| l.record.kind() == "prerequisite")
        .unwrap() as u64;
    person(
        &t,
        WorkRecord::Accepted {
            item: g.clone(),
            tier: Some(2),
            triage_skipped: None,
        },
    )
    .await;
    assert_eq!(
        claimed_id(&claim(&t, "w2").await).as_deref(),
        Some(g.as_str())
    );
    let mut d = decide_args();
    d.evidence = vec![Evidence {
        stream: WORK_EVENTS.into(),
        offset: origin,
    }];
    t.decide(&agent("w2"), &g, d).await.unwrap();
}

/// `a1` with its triage item held by w1, who may reach `repo`.
async fn triaging(repo: &str) -> (Fake, Tracker<Fake>) {
    let fake = Fake::default();
    let grants = [("w1".to_owned(), vec![github(repo)])].into();
    let t = start_with(
        &fake,
        crate::tracker::Settings {
            grants,
            ..settings()
        },
    )
    .await;
    // Triage is claimed before `a1` is accepted, so `a1` still has every try.
    let record = |r| async { t.person_record(key("gab"), r).await.unwrap() };
    record(WorkRecord::Imported {
        item: item("a1"),
        source: "gh".into(),
        upstream_id: "a1".into(),
        title: "fix a1".into(),
        labels: vec![],
        milestone: None,
    })
    .await;
    record(WorkRecord::Proposed {
        item: item("a1-t"),
        parent: Some(item("a1")),
        key: key("triage"),
        work: WorkKind::Triage,
        title: "Triage: fix a1".into(),
        review: None,
    })
    .await;
    claim_item(&t, "w1", &item("a1-t")).await;
    record(WorkRecord::Accepted {
        item: item("a1"),
        tier: Some(2),
        triage_skipped: None,
    })
    .await;
    (fake, t)
}

#[tokio::test]
async fn triage_posts_a_prerequisite_before_any_build_claim() {
    let (fake, t) = triaging("GetFelix/felix-gateway").await;
    let (w1, a1, tr) = (agent("w1"), item("a1"), item("a1-t"));
    let post = || {
        t.prerequisite(
            &w1,
            &tr,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Merged,
            Vec::new(),
        )
    };
    // Format 3: refused until a person raises the log, and nothing waits.
    assert_eq!(why(post().await), "format");
    assert_eq!(stage(&t, &a1).await, Some(Stage::Ready));
    person(&t, WorkRecord::FormatRaised { to: 3 }).await;

    let reply = post().await.unwrap();
    assert_eq!(reply["item"], "a1");
    assert_eq!(reply["status"], "waiting");
    assert_eq!(reply["claim"], "held");
    assert_eq!(reply["created"], true);
    let g = item(reply["prerequisite"].as_str().unwrap());
    assert_eq!(stage(&t, &g).await, Some(Stage::Proposed));
    // The item under triage waits, so no build claim is handed out.
    assert_eq!(stage(&t, &a1).await, Some(Stage::Blocked));
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    // The record is format 3 and charges nothing: triage has not ended.
    let last = fake.log().pop().unwrap();
    assert_eq!(last.v, 3);
    assert!(matches!(
        &last.record,
        WorkRecord::Prerequisite { item, triage: Some(by), cost_micros: 0, .. }
            if *item == a1 && *by == tr
    ));
    // A retry answers with the first call's work and writes nothing.
    let before = fake.log().len();
    let again = post().await.unwrap();
    assert_eq!(again["created"], false);
    assert_eq!(again["prerequisite"], reply["prerequisite"]);
    assert_eq!(fake.log().len(), before);
    // The same key with another `done_when` is refused, not answered as done.
    let other = t.prerequisite(
        &w1,
        &tr,
        None,
        key("cas"),
        gateway(),
        DoneWhen::Released,
        Vec::new(),
    );
    assert_eq!(why(other.await), "invalid");
    assert_eq!(fake.log().len(), before);

    // Triage still holds its claim and finishes as usual.
    t.triage(
        &w1,
        &tr,
        ans(1, "needs the gateway first".into(), None, None),
    )
    .await
    .unwrap();
    t.complete(&w1, &tr, None, Vec::new()).await.unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    // Once the other repo's work is merged the item is handed out.
    person(
        &t,
        WorkRecord::Accepted {
            item: g.clone(),
            tier: None,
            triage_skipped: None,
        },
    )
    .await;
    person(
        &t,
        WorkRecord::Closed {
            item: g,
            why: CloseWhy::Merged,
        },
    )
    .await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

/// From format 5 a merged ask on a link that waits for a release changes
/// nothing, so the tracker answers it without writing a record.
#[tokio::test]
async fn a_merged_ask_after_a_released_one_writes_nothing_from_format_5() {
    let (fake, t) = triaging("GetFelix/felix-gateway").await;
    let (w1, tr) = (agent("w1"), item("a1-t"));
    person(&t, WorkRecord::FormatRaised { to: 3 }).await;
    let reply = t
        .prerequisite(
            &w1,
            &tr,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Released,
            Vec::new(),
        )
        .await
        .unwrap();
    let g = item(reply["prerequisite"].as_str().unwrap());
    let before = fake.log().len();
    let merged = || {
        t.prerequisite(
            &w1,
            &tr,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Merged,
            Vec::new(),
        )
    };
    assert_eq!(why(merged().await), "invalid");
    person(&t, WorkRecord::FormatRaised { to: 5 }).await;
    let before = before + 1;
    assert_eq!(merged().await.unwrap()["created"], false);
    let existing = t.prerequisite(
        &w1,
        &tr,
        None,
        key("cas-2"),
        Prereq::Existing(g),
        DoneWhen::Merged,
        Vec::new(),
    );
    assert_eq!(existing.await.unwrap()["created"], false);
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn a_triage_prerequisite_names_only_work_the_agent_may_reach() {
    let (fake, t) = triaging("GetFelix/felix").await;
    let (w1, a1, tr) = (agent("w1"), item("a1"), item("a1-t"));
    seed_in_felix(&t, "a2").await;
    person(&t, WorkRecord::FormatRaised { to: 3 }).await;
    let existing = |id: &str| {
        t.prerequisite(
            &w1,
            &tr,
            None,
            key("dep"),
            Prereq::Existing(item(id)),
            DoneWhen::Merged,
            Vec::new(),
        )
    };
    let before = fake.log().len();
    assert_eq!(why(existing("a1-t").await), "invalid");
    assert_eq!(why(existing("nope").await), "unknown_item");
    let other = t.prerequisite(
        &w1,
        &tr,
        None,
        key("cas"),
        gateway(),
        DoneWhen::Merged,
        Vec::new(),
    );
    assert_eq!(why(other.await), "no_grant");
    // Only the holder of the triage item.
    let w2 = agent("w2");
    let stranger = t.prerequisite(
        &w2,
        &tr,
        None,
        key("dep"),
        Prereq::Existing(item("a2")),
        DoneWhen::Merged,
        Vec::new(),
    );
    assert_eq!(why(stranger.await), "not_held");
    assert_eq!(fake.log().len(), before);
    assert_eq!(stage(&t, &a1).await, Some(Stage::Ready));

    let reply = existing("a2").await.unwrap();
    assert_eq!(reply["status"], "waiting");
    assert_eq!(reply["created"], false);
    assert_eq!(stage(&t, &a1).await, Some(Stage::Blocked));
    // The same call again does not use up a post.
    let before = fake.log().len();
    existing("a2").await.unwrap();
    assert_eq!(fake.log().len(), before);
    // Asking to wait for the release instead is a change, so it is written.
    t.prerequisite(
        &w1,
        &tr,
        None,
        key("dep"),
        Prereq::Existing(item("a2")),
        DoneWhen::Released,
        Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(fake.log().len(), before + 1);
    let core = t.0.core.lock().await;
    assert!(core.state.release_edges.contains(&(a1, item("a2"))));
}

/// From format 6 ingest may tie a prerequisite to the issue it opened, but
/// only once a person accepted it after the raise, and only in its target repo.
#[tokio::test]
async fn ingest_takes_an_issue_only_for_a_prerequisite_a_person_accepted() {
    let (_fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    person(&t, WorkRecord::FormatRaised { to: 6 }).await;
    let reply = t
        .prerequisite(
            &w1,
            &a1,
            None,
            key("cas"),
            gateway(),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await
        .unwrap();
    let g = id_of(&json!({ "item": reply["prerequisite"] }));
    let attach = |up: &str| WorkRecord::Imported {
        item: g.clone(),
        source: "github".into(),
        upstream_id: up.into(),
        title: "gateway needs a CAS call".into(),
        labels: vec![],
        milestone: None,
    };
    let refused = |e: Error| match e {
        Error::Refused { why } => why,
        e => panic!("{e}"),
    };
    let listed = t.ingest_roots(None).await.unwrap()["prerequisites"].clone();
    assert_eq!(listed, json!([]), "not accepted yet");
    let err = t
        .ingest_record(key("ingest-gh"), attach("GetFelix/felix-gateway#5"))
        .await
        .unwrap_err();
    assert_eq!(refused(err), "actor_not_allowed");

    person(
        &t,
        WorkRecord::Accepted {
            item: g.clone(),
            tier: None,
            triage_skipped: None,
        },
    )
    .await;
    let listed = t.ingest_roots(None).await.unwrap()["prerequisites"].clone();
    assert_eq!(
        listed,
        json!([{
            "item": g, "title": "gateway needs a CAS call", "source": "github",
            "repo": "GetFelix/felix-gateway", "why": "the client cannot swap atomically",
            "needed_by": "GetFelix/felix#1",
        }])
    );
    let err = t
        .ingest_record(key("ingest-gh"), attach("GetFelix/felix#5"))
        .await
        .unwrap_err();
    assert_eq!(refused(err), "duplicate", "not its target repo");
    t.ingest_record(key("ingest-gh"), attach("GetFelix/felix-gateway#5"))
        .await
        .unwrap();
    let roots = t.ingest_roots(None).await.unwrap();
    assert_eq!(roots["prerequisites"], json!([]));
    assert!(
        roots["roots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["upstream_id"] == "GetFelix/felix-gateway#5")
    );
    // Now an imported root, so upstream sync may close it.
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::Closed {
            item: g.clone(),
            why: kbl_core::work::CloseWhy::Completed,
        },
    )
    .await
    .unwrap();
    assert_eq!(stage(&t, &a1).await, Some(Stage::Ready));
}

/// Ingest links `item` to `to`, then unlinks it, as upstream adding and then
/// dropping the dependency would. Neither may touch an edge ingest did not make.
async fn ingest_cannot_take_over(fake: &Fake, item: &ItemId, to: &ItemId) {
    let ingest = |t: Tracker<Fake>, r| async move { t.ingest_record(key("ingest-gh"), r).await };
    let edge = (item.clone(), to.clone());
    for t in [start(fake).await, start(fake).await] {
        let before = fake.log().len();
        let linked = WorkRecord::Linked {
            item: item.clone(),
            to: to.clone(),
        };
        let unlinked = WorkRecord::Unlinked {
            item: item.clone(),
            to: to.clone(),
        };
        assert_eq!(why(ingest(t.clone(), linked).await), "already_linked");
        assert_eq!(why(ingest(t.clone(), unlinked).await), "not_ingests");
        assert_eq!(fake.log().len(), before);
        assert!(t.0.core.lock().await.state.edges.contains(&edge));
    }
}

#[tokio::test]
async fn ingest_cannot_unlink_an_agents_prerequisite() {
    let (fake, t) = granted().await;
    let (w1, a1, f2) = (agent("w1"), item("a1"), item("f2"));
    person(&t, WorkRecord::FormatRaised { to: 5 }).await;
    import(&t, "f2", "GetFelix/felix-gateway#4").await;
    t.prerequisite(
        &w1,
        &a1,
        None,
        key("k"),
        Prereq::Existing(f2.clone()),
        DoneWhen::Released,
        Vec::new(),
    )
    .await
    .unwrap();
    drop(t);
    ingest_cannot_take_over(&fake, &a1, &f2).await;
    let t = start(&fake).await;
    assert!(
        t.0.core
            .lock()
            .await
            .state
            .release_edges
            .contains(&(a1, f2))
    );
}

/// Ingest links first, then an agent names the same edge as a prerequisite
/// that waits for a release: the edge is the agent's from then on.
#[tokio::test]
async fn an_agents_prerequisite_takes_over_an_edge_ingest_made() {
    let (fake, t) = granted().await;
    let (w1, a1, f2) = (agent("w1"), item("a1"), item("f2"));
    person(&t, WorkRecord::FormatRaised { to: 5 }).await;
    import(&t, "f2", "GetFelix/felix-gateway#4").await;
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::Linked {
            item: a1.clone(),
            to: f2.clone(),
        },
    )
    .await
    .unwrap();
    person(
        &t,
        WorkRecord::Closed {
            item: f2.clone(),
            why: CloseWhy::Merged,
        },
    )
    .await;
    t.prerequisite(
        &w1,
        &a1,
        None,
        key("k"),
        Prereq::Existing(f2.clone()),
        DoneWhen::Released,
        Vec::new(),
    )
    .await
    .unwrap();
    // Live, before any restart rebuilds what ingest made.
    let live = t
        .ingest_record(
            key("ingest-gh"),
            WorkRecord::Unlinked {
                item: a1.clone(),
                to: f2.clone(),
            },
        )
        .await;
    assert_eq!(why(live), "not_ingests");
    drop(t);
    ingest_cannot_take_over(&fake, &a1, &f2).await;
    let t = start(&fake).await;
    let core = t.0.core.lock().await;
    assert!(core.state.release_edges.contains(&(a1.clone(), f2.clone())));
    assert_eq!(core.state.stage(&a1), Some(Stage::Blocked));
}

/// The same from triage, where the waiter is the item under triage.
#[tokio::test]
async fn a_triage_prerequisite_takes_over_an_edge_ingest_made() {
    let (fake, t) = triaging("GetFelix/felix-gateway").await;
    let (w1, a1, tr, f2) = (agent("w1"), item("a1"), item("a1-t"), item("f2"));
    person(&t, WorkRecord::FormatRaised { to: 5 }).await;
    import(&t, "f2", "GetFelix/felix-gateway#4").await;
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::Linked {
            item: a1.clone(),
            to: f2.clone(),
        },
    )
    .await
    .unwrap();
    person(
        &t,
        WorkRecord::Closed {
            item: f2.clone(),
            why: CloseWhy::Merged,
        },
    )
    .await;
    t.prerequisite(
        &w1,
        &tr,
        None,
        key("k"),
        Prereq::Existing(f2.clone()),
        DoneWhen::Released,
        Vec::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        &fake.log().last().unwrap().record,
        WorkRecord::Prerequisite {
            triage: Some(_),
            ..
        }
    ));
    drop(t);
    ingest_cannot_take_over(&fake, &a1, &f2).await;
    let t = start(&fake).await;
    assert!(
        t.0.core
            .lock()
            .await
            .state
            .release_edges
            .contains(&(a1, f2))
    );
}

#[tokio::test]
async fn ingest_cannot_unlink_a_prerequisite_from_an_answer() {
    let (fake, t, _q) = asked_with_options().await;
    person(&t, WorkRecord::FormatRaised { to: 6 }).await;
    let moves = Reply {
        moves_to: Some((
            Target {
                source: "github".into(),
                repo: "GetFelix/felix-gateway".into(),
            },
            "gateway needs a CAS call".into(),
        )),
        ..Reply::default()
    };
    t.person_answer(
        key("gab"),
        &item("a1"),
        None,
        text("belongs in felix-gateway"),
        vec![],
        moves,
    )
    .await
    .unwrap();
    let p =
        t.0.core
            .lock()
            .await
            .state
            .edges
            .iter()
            .next()
            .unwrap()
            .1
            .clone();
    person(
        &t,
        WorkRecord::Accepted {
            item: p.clone(),
            tier: None,
            triage_skipped: None,
        },
    )
    .await;
    // Ingest opens its issue upstream and imports it.
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::Imported {
            item: p.clone(),
            source: "github".into(),
            upstream_id: "GetFelix/felix-gateway#40".into(),
            title: "gateway needs a CAS call".into(),
            labels: vec![],
            milestone: None,
        },
    )
    .await
    .unwrap();
    drop(t);
    ingest_cannot_take_over(&fake, &item("a1"), &p).await;
}

#[tokio::test]
async fn a_prerequisite_retried_after_it_ended_the_claim_is_replayed() {
    let (fake, t) = granted().await;
    let (w1, a1) = (agent("w1"), item("a1"));
    import(&t, "f2", "GetFelix/felix-gateway#4").await;
    let ask = |attempt| {
        t.prerequisite(
            &w1,
            &a1,
            attempt,
            key("k"),
            Prereq::Existing(item("f2")),
            DoneWhen::Merged,
            Vec::new(),
        )
    };
    let claimed_at = fake.kinds().iter().rposition(|k| k == "claimed").unwrap() as u64;
    let first = ask(Some(crate::tracker::Which::Attempt(1))).await.unwrap();
    let at = fake.log().len() as u64 - 1;
    assert_eq!(first["offset"], at);
    let kinds = fake.kinds();
    let again = ask(Some(crate::tracker::Which::Attempt(1))).await.unwrap();
    assert_eq!(again["replayed"], true);
    assert_eq!(again["offset"], at);
    let again = ask(Some(crate::tracker::Which::Claim(claimed_at)))
        .await
        .unwrap();
    assert_eq!(
        (&again["replayed"], &again["offset"]),
        (&json!(true), &json!(at))
    );
    assert_eq!(fake.kinds(), kinds);
    // A retry naming other work is not the call that landed.
    import(&t, "f3", "GetFelix/felix-gateway#5").await;
    let other = t
        .prerequisite(
            &w1,
            &a1,
            Some(crate::tracker::Which::Attempt(1)),
            key("k"),
            Prereq::Existing(item("f3")),
            DoneWhen::Merged,
            Vec::new(),
        )
        .await;
    assert!(matches!(other, Err(Error::NotHeld(_))));
    let new = Prereq::New {
        target: Target {
            source: "github".into(),
            repo: "GetFelix/felix-gateway".into(),
        },
        title: "x".into(),
        why: text("y"),
        evidence: vec![],
    };
    let other = t
        .prerequisite(
            &w1,
            &a1,
            Some(crate::tracker::Which::Attempt(1)),
            key("k2"),
            new,
            DoneWhen::Merged,
            Vec::new(),
        )
        .await;
    assert!(matches!(other, Err(Error::NotHeld(_))));
    // Another attempt, or no attempt at all, is not the call that landed.
    assert!(matches!(
        ask(Some(crate::tracker::Which::Attempt(2))).await,
        Err(Error::NotHeld(_))
    ));
    assert!(matches!(ask(None).await, Err(Error::NotHeld(_))));
}

#[tokio::test]
async fn a_key_that_hits_a_closed_or_rejected_item_says_so() {
    let (_fake, t) = held().await;
    let first = post(&t, "w1", "a1", "p", "proposal").await.unwrap();
    assert_eq!(first["created"], true);
    let open = post(&t, "w1", "a1", "p", "proposal").await.unwrap();
    assert_eq!(
        (open["created"].clone(), open["stage"].clone()),
        (false.into(), "proposed".into())
    );
    assert_eq!(open["closure"], Value::Null);
    t.person_record(
        key("gab"),
        WorkRecord::Rejected {
            item: item(first["item"].as_str().unwrap()),
        },
    )
    .await
    .unwrap();
    let dead = post(&t, "w1", "a1", "p", "proposal").await.unwrap();
    assert_eq!(dead["created"], false);
    assert_eq!(dead["stage"], "closed");
    assert_eq!(dead["closure"], "rejected");
}

#[tokio::test]
async fn proposals_waiting_use_up_the_root_budget_until_a_person_decides() {
    let (fake, t) = held().await;
    t.person_record(
        key("gab"),
        WorkRecord::BudgetSet {
            root: item("a1"),
            items_left: 2,
            max_depth: 3,
            cost_left_micros: 1_000_000,
            redos_per_item: 3,
        },
    )
    .await
    .unwrap();
    let a = post(&t, "w1", "a1", "p0", "proposal").await.unwrap();
    post(&t, "w1", "a1", "p1", "proposal").await.unwrap();
    let before = count(&fake, "proposed");
    assert_eq!(
        why(post(&t, "w1", "a1", "p2", "proposal").await),
        "over_budget"
    );
    assert_eq!(count(&fake, "proposed"), before);
    // The proposals took nothing from the budget itself.
    assert_eq!(
        t.0.core.lock().await.state.budgets[&item("a1")].items_left,
        2
    );
    // A rejection frees the room.
    t.person_record(
        key("gab"),
        WorkRecord::Rejected {
            item: item(a["item"].as_str().unwrap()),
        },
    )
    .await
    .unwrap();
    post(&t, "w1", "a1", "p2", "proposal").await.unwrap();
}

#[tokio::test]
async fn texts_past_the_cap_are_read_back_from_the_log() {
    let (_fake, t) = held().await;
    for n in 0..4 {
        t.context_add(
            &agent("w1"),
            &item("a1"),
            key(&format!("e{n}")),
            text(&format!("{n}{}", "x".repeat(100))),
        )
        .await
        .unwrap();
    }
    t.0.core.lock().await.recall.texts.set_cap(150);
    // The cap bites on the next insert.
    t.context_add(&agent("w1"), &item("a1"), key("e4"), text(&"y".repeat(100)))
        .await
        .unwrap();
    {
        let core = t.0.core.lock().await;
        assert!(core.recall.texts.bytes() <= 150);
        assert!(core.recall.texts.peek(1).is_none() || core.recall.texts.peek(2).is_none());
    }
    let read = t.context_read(&agent("w1"), &item("a1")).await.unwrap();
    let entries = read["context"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    for (n, e) in entries.iter().enumerate().take(4) {
        assert_eq!(e["text"], json!(text(&format!("{n}{}", "x".repeat(100)))));
    }
}

#[tokio::test]
async fn a_steer_reads_back_to_the_holder_as_a_note_even_after_a_restart() {
    let (fake, t) = held().await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 12 })
        .await
        .unwrap();
    let steer = |s: &str| WorkRecord::Steered {
        item: item("a1"),
        attempt: 1,
        text: text(s),
    };
    // Too long to keep inline: it needs an artifact, as a note does.
    let big = "x".repeat(kbl_core::artifact::INLINE_LIMIT + 1);
    assert_eq!(
        why(t.person_record(key("gab"), steer(&big)).await),
        "invalid"
    );
    t.person_record(key("gab"), steer("use the writer lock"))
        .await
        .unwrap();
    let check = |v: &Value| {
        assert_eq!(v["notes"].as_array().unwrap().len(), 1, "{v}");
        assert_eq!(v["notes"][0]["text"]["text"], "use the writer lock");
    };
    check(&t.context_read(&agent("w1"), &item("a1")).await.unwrap());
    // From the log, not the cache.
    let t2 = start(&fake).await;
    check(&t2.context_read(&agent("w1"), &item("a1")).await.unwrap());
}
