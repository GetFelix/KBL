use super::*;
use crate::codec::Codec;
use crate::content::Content;
use crate::work::{HumanKind, Phase};

fn id(s: &str) -> ItemId {
    ItemId::new(s).unwrap()
}
fn key(s: &str) -> Key {
    Key::new(s).unwrap()
}
fn text(s: &str) -> Content {
    Content::Text(s.into())
}

/// Offsets count up from zero, one per record.
#[derive(Default)]
struct Log(Vec<(u64, WorkLogged)>);

impl Log {
    fn push(&mut self, by: &str, actor: Actor, record: WorkRecord) -> u64 {
        let offset = self.0.len() as u64;
        self.0.push((
            offset,
            WorkLogged {
                at_ms: offset * 10,
                by: key(by),
                actor,
                record,
            },
        ));
        offset
    }
    fn agent(&mut self, by: &str, record: WorkRecord) -> u64 {
        self.push(by, Actor::Agent, record)
    }
    fn person(&mut self, record: WorkRecord) -> u64 {
        self.push("gabriel", Actor::Person, record)
    }
    fn tracker(&mut self, record: WorkRecord) -> u64 {
        self.push("tracker", Actor::Tracker, record)
    }
}

fn imported(item: &str) -> WorkRecord {
    WorkRecord::Imported {
        item: id(item),
        source: "github".into(),
        upstream_id: format!("felix#{item}"),
        title: format!("bug {item}"),
    }
}
fn proposed(item: &str, parent: Option<&str>, k: &str, work: WorkKind) -> WorkRecord {
    WorkRecord::Proposed {
        item: id(item),
        parent: parent.map(id),
        key: key(k),
        work,
        title: item.into(),
    }
}
fn claimed(item: &str, attempt: u32) -> WorkRecord {
    WorkRecord::Claimed {
        item: id(item),
        attempt,
        deadline_ms: 90_000,
    }
}
fn decision(item: &str, attempt: u32, evidence: Vec<Evidence>) -> WorkRecord {
    WorkRecord::Decision {
        item: id(item),
        attempt,
        phase: Phase::Cause,
        so_far: "the cache is read before it is filled".into(),
        question: "where does it go wrong".into(),
        options: vec!["reader".into(), "writer".into()],
        chosen: "reader".into(),
        why: "the writer holds the lock".into(),
        commit: "abc123".into(),
        evidence,
    }
}
fn done(item: &str, attempt: u32, cost_micros: u64) -> WorkRecord {
    WorkRecord::Done {
        item: id(item),
        attempt,
        cost_micros,
    }
}
fn evidence(offset: u64) -> Evidence {
    Evidence {
        stream: WORK_EVENTS.into(),
        offset,
    }
}

/// A root, accepted, claimed by w1, with two children, an edge, a decision, a
/// step, an edit, a person's note, a fork and a second attempt.
fn story() -> Log {
    let mut l = Log::default();
    l.tracker(imported("r1")); // 0
    l.tracker(proposed("t1", Some("r1"), "triage", WorkKind::Triage)); // 1
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(1),
    }); // 2
    l.agent("w1", claimed("r1", 1)); // 3
    l.agent("w1", proposed("c1", Some("r1"), "a", WorkKind::Split)); // 4
    l.agent("w1", proposed("c2", Some("r1"), "b", WorkKind::Test)); // 5
    l.tracker(WorkRecord::Linked {
        item: id("c2"),
        to: id("c1"),
    }); // 6
    let d = l.agent("w1", decision("r1", 1, vec![])); // 7
    l.agent(
        "w1",
        WorkRecord::Step {
            item: id("r1"),
            attempt: 1,
            command: "cargo test".into(),
            exit_code: 1,
            output: text("1 failed"),
        },
    ); // 8
    l.agent(
        "w1",
        WorkRecord::Edit {
            item: id("r1"),
            attempt: 1,
            file: "src/a.rs".into(),
            hunk: text("@@"),
            decision: d,
        },
    ); // 9
    l.person(WorkRecord::HumanInput {
        item: id("r1"),
        attempt: Some(1),
        input: HumanKind::ReviewComment,
        text: text("not in the reader"),
    }); // 10
    l.agent(
        "w1",
        WorkRecord::GaveUp {
            item: id("r1"),
            attempt: 1,
            reason: "wrong path".into(),
            cost_micros: 2_000_000,
        },
    ); // 11
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: d,
        note: Some(10),
    }); // 12
    l.agent("w2", claimed("r1", 2)); // 13
    l.agent("w2", done("c1", 1, 0)); // 14: w2 does not hold c1, refused
    l.agent("w3", claimed("c1", 1)); // 15
    l.agent("w3", done("c1", 1, 500_000)); // 16
    l.agent("w2", done("r1", 2, 1_000_000)); // 17: c2 still open, refused
    l
}

#[test]
fn the_story_folds_to_the_expected_items() {
    let s = fold(&story().0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Held));
    assert_eq!(s.stage(&id("c1")), Some(Stage::Done));
    // c2 waited on c1, which is done.
    assert_eq!(s.stage(&id("c2")), Some(Stage::Ready));
    assert_eq!(s.stage(&id("t1")), Some(Stage::Ready));
    let r1 = &s.items[&id("r1")];
    assert_eq!(r1.attempts, 2);
    assert_eq!(r1.paths.len(), 2);
    assert_eq!(
        r1.paths[1].forked_from,
        Some(Fork {
            from_attempt: 1,
            at: 7,
            note: Some(10)
        })
    );
    assert_eq!(s.open_children(&id("r1")), vec![&id("t1"), &id("c2")]);
    assert_eq!(
        s.rejections
            .iter()
            .map(|r| (r.offset, r.why))
            .collect::<Vec<_>>(),
        vec![(14, Reject::NotHolder), (17, Reject::OpenChildren)]
    );
    let b = &s.budgets[&id("r1")];
    // Two children drawn from the root, one fork, 2.5 dollars of cost.
    assert_eq!((b.items_left, b.redos_left), (9, 2));
    assert_eq!(b.cost_left_micros, DEFAULT_COST_MICROS - 2_500_000);
}

#[test]
fn a_seek_equals_the_live_state_at_every_offset() {
    let log = story().0;
    let seek = Seek::build(&log, 4);
    let mut live = State::default();
    for (i, (o, r)) in log.iter().enumerate() {
        live.apply(*o, r);
        assert_eq!(seek.state_at(&log, *o), live, "at offset {o}");
        assert_eq!(fold(&log[..=i]), live);
    }
}

#[test]
fn the_fold_at_n_uses_no_record_after_n() {
    let log = story().0;
    let seek = Seek::build(&log, 4);
    for n in 0..log.len() as u64 {
        // Everything after n is replaced by records that would change the state.
        let mut poisoned = log.clone();
        for (o, rec) in poisoned.iter_mut().filter(|(o, _)| *o > n) {
            rec.record = WorkRecord::Done {
                item: id("r1"),
                attempt: *o as u32,
                cost_micros: 99,
            };
            rec.actor = Actor::Person;
        }
        let want = fold(&log[..=n as usize]);
        assert_eq!(fold(poisoned.iter().take_while(|(o, _)| *o <= n)), want);
        assert_eq!(seek.state_at(&log, n), want);
    }
}

#[test]
fn a_snapshot_survives_json_and_keeps_deduping() {
    let log = story().0;
    let mut restored: State = State::decode(&fold(&log[..8]).encode()).unwrap();
    for (o, r) in &log[8..] {
        restored.apply(*o, r);
    }
    assert_eq!(restored, fold(&log));
    // The child key and the upstream id were remembered across the round trip.
    let mut more = Log(log.clone());
    more.agent("w2", proposed("c9", Some("r1"), "a", WorkKind::Split));
    more.tracker(WorkRecord::Imported {
        item: id("r9"),
        source: "github".into(),
        upstream_id: "felix#r1".into(),
        title: "again".into(),
    });
    for (o, r) in &more.0[log.len()..] {
        restored.apply(*o, r);
    }
    let last: Vec<_> = restored
        .rejections
        .iter()
        .rev()
        .take(2)
        .map(|r| r.why)
        .collect();
    assert_eq!(last, vec![Reject::Duplicate, Reject::Duplicate]);
}

#[test]
fn agent_records_never_raise_a_budget() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    let raise = |n: u32| WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 1000 + n,
        max_depth: 9,
        cost_left_micros: u64::MAX,
        redos_left: 9,
    };
    l.agent("w1", raise(0));
    l.agent("w1", proposed("c1", Some("r1"), "a", WorkKind::Split));
    l.agent("w1", raise(1));
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("plan"),
            branch: Some("fix".into()),
            pr: None,
        },
    );
    l.agent(
        "w1",
        WorkRecord::GaveUp {
            item: id("r1"),
            attempt: 1,
            reason: "x".into(),
            cost_micros: 700,
        },
    );
    l.tracker(raise(2));
    let mut prev: Option<Budget> = None;
    let mut s = State::default();
    for (o, r) in &l.0 {
        s.apply(*o, r);
        let b = s.budgets.get(&id("r1")).cloned();
        if let (Some(p), Some(b)) = (&prev, &b) {
            assert!(
                b.items_left <= p.items_left
                    && b.cost_left_micros <= p.cost_left_micros
                    && b.redos_left <= p.redos_left
                    && b.max_depth <= p.max_depth,
                "budget rose at offset {o}"
            );
        }
        prev = b;
    }
    assert_eq!(s.budgets[&id("r1")].items_left, DEFAULT_MAX_ITEMS - 2);
    // A person can raise it.
    l.person(raise(3));
    s.apply(l.0.len() as u64 - 1, &l.0.last().unwrap().1);
    assert_eq!(s.budgets[&id("r1")].items_left, 1003);
}

#[test]
fn a_child_without_a_held_parent_is_refused() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    // Nobody holds r1 yet.
    l.agent("w1", proposed("c1", Some("r1"), "a", WorkKind::Split));
    l.agent("w1", claimed("r1", 1));
    // Held, but by w1, not w2.
    l.agent("w2", proposed("c2", Some("r1"), "b", WorkKind::Split));
    // An agent cannot make a root of a live kind.
    l.agent("w1", proposed("c3", None, "c", WorkKind::Test));
    l.agent("w1", proposed("c4", Some("r1"), "d", WorkKind::Split));
    let s = fold(&l.0);
    assert_eq!(
        s.rejections
            .iter()
            .map(|r| (r.offset, r.why))
            .collect::<Vec<_>>(),
        vec![
            (2, Reject::NoHeldParent),
            (4, Reject::NoHeldParent),
            (5, Reject::NoHeldParent)
        ]
    );
    assert_eq!(
        s.items.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
        vec!["c4", "r1"]
    );
}

#[test]
fn unknown_kinds_are_skipped() {
    let line =
        br#"{"at_ms":5,"by":"tracker","actor":"tracker","kind":"snoozed","item":"r1","hours":4}"#;
    let rec = WorkLogged::decode(line).unwrap();
    assert_eq!(rec.record, WorkRecord::Unknown);
    let mut l = story();
    let plain = fold(&l.0);
    l.0.insert(5, (5, rec.clone()));
    for (i, e) in l.0.iter_mut().enumerate() {
        e.0 = i as u64;
    }
    let mut with = Log::default();
    // The same story with a record the fold does not know in the middle.
    for (i, (_, r)) in l.0.iter().enumerate() {
        if i == 5 {
            with.0.push((5, rec.clone()));
        } else {
            with.0.push((with.0.len() as u64, r.clone()));
        }
    }
    let s = fold(&with.0);
    assert!(
        s.rejections.iter().all(|r| r.offset != 5),
        "unknown kind was rejected"
    );
    assert_eq!(s.items.len(), plain.items.len());
}

#[test]
fn budgets_cap_items_and_depth() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    for i in 0..13 {
        l.agent(
            "w1",
            proposed(
                &format!("c{i}"),
                Some("r1"),
                &format!("k{i}"),
                WorkKind::Augment,
            ),
        );
    }
    let s = fold(&l.0);
    // The root plus 11 children is the 12 the tree allows.
    assert_eq!(s.items.len(), 12);
    assert_eq!(s.rejections.len(), 2);
    assert!(s.rejections.iter().all(|r| r.why == Reject::OverBudget));

    // Depth: a held child's own child is depth 2, and so on to the cap.
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    let mut parent = "r1".to_string();
    for d in 1..=4u32 {
        let who = format!("w{d}");
        l.agent(&who, claimed(&parent, 1));
        let child = format!("d{d}");
        l.agent(&who, proposed(&child, Some(&parent), "k", WorkKind::Split));
        parent = child;
    }
    let s = fold(&l.0);
    assert!(s.items.contains_key(&id("d3")));
    assert!(!s.items.contains_key(&id("d4")));
    assert_eq!(s.rejections.last().unwrap().why, Reject::TooDeep);
}

#[test]
fn people_children_are_free_and_agent_fixes_are_proposals() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.person(proposed("p1", Some("r1"), "x", WorkKind::Fix));
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", proposed("bug2", None, "found", WorkKind::Fix));
    let s = fold(&l.0);
    assert_eq!(s.budgets[&id("r1")].items_left, DEFAULT_MAX_ITEMS - 1);
    assert_eq!(s.stage(&id("p1")), Some(Stage::Ready));
    assert_eq!(s.stage(&id("bug2")), Some(Stage::Proposed));
    assert!(s.budgets.contains_key(&id("bug2")));
}

#[test]
fn a_cycle_is_refused_and_a_dependency_blocks() {
    let mut l = Log::default();
    for r in ["a", "b", "c"] {
        l.tracker(imported(r));
        l.person(WorkRecord::Accepted {
            item: id(r),
            tier: None,
        });
    }
    l.tracker(WorkRecord::Linked {
        item: id("a"),
        to: id("b"),
    });
    l.tracker(WorkRecord::Linked {
        item: id("b"),
        to: id("c"),
    });
    l.tracker(WorkRecord::Linked {
        item: id("c"),
        to: id("a"),
    });
    let s = fold(&l.0);
    assert_eq!(s.rejections.len(), 1);
    assert_eq!(s.rejections[0].why, Reject::Cycle);
    assert_eq!(s.stage(&id("a")), Some(Stage::Blocked));
    assert_eq!(s.stage(&id("c")), Some(Stage::Ready));
}

#[test]
fn a_decision_cannot_cite_the_future_or_skip_its_fields() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", decision("r1", 1, vec![evidence(3)])); // 3 is itself
    l.agent("w1", decision("r1", 1, vec![evidence(2)]));
    l.agent(
        "w1",
        WorkRecord::Decision {
            item: id("r1"),
            attempt: 1,
            phase: Phase::Fix,
            so_far: String::new(),
            question: "q".into(),
            options: vec![],
            chosen: "c".into(),
            why: "w".into(),
            commit: "abc".into(),
            evidence: vec![],
        },
    );
    l.agent(
        "w1",
        WorkRecord::Edit {
            item: id("r1"),
            attempt: 1,
            file: "f".into(),
            hunk: text("h"),
            decision: 2,
        },
    );
    let s = fold(&l.0);
    let why: Vec<_> = s.rejections.iter().map(|r| r.why).collect();
    assert_eq!(
        why,
        vec![
            Reject::EvidenceFromTheFuture,
            Reject::MissingField,
            Reject::UnknownDecision
        ]
    );
    assert_eq!(s.items[&id("r1")].paths[0].decisions, vec![4]);
}

#[test]
fn human_inputs_follow_the_path_up_to_the_fork() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1)); // 2
    let before = l.person(WorkRecord::HumanInput {
        item: id("r1"),
        attempt: Some(1),
        input: HumanKind::Note,
        text: text("early"),
    }); // 3
    let d = l.agent("w1", decision("r1", 1, vec![])); // 4
    l.agent("w1", decision("r1", 1, vec![])); // 5
    let after = l.person(WorkRecord::HumanInput {
        item: id("r1"),
        attempt: Some(1),
        input: HumanKind::Note,
        text: text("late, after the fork point"),
    }); // 6
    let whole = l.person(WorkRecord::HumanInput {
        item: id("r1"),
        attempt: None,
        input: HumanKind::PriorityChange,
        text: text("p0"),
    }); // 7
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
        },
    );
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: d,
        note: Some(before),
    });
    l.agent("w2", claimed("r1", 2));
    let steer = l.person(WorkRecord::HumanInput {
        item: id("r1"),
        attempt: Some(2),
        input: HumanKind::Note,
        text: text("take the writer"),
    });
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!(
        s.human_inputs_on_path(&id("r1"), 1),
        vec![before, after, whole]
    );
    // The second attempt does not inherit what was said after the fork point.
    assert_eq!(
        s.human_inputs_on_path(&id("r1"), 2),
        vec![before, whole, steer]
    );
}

#[test]
fn redos_are_limited_and_a_fork_needs_a_real_decision() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    let mut ds = Vec::new();
    for a in 1..=5u32 {
        l.agent("w1", claimed("r1", a));
        ds.push(l.agent("w1", decision("r1", a, vec![])));
        l.agent(
            "w1",
            WorkRecord::Released {
                item: id("r1"),
                attempt: a,
                reason: None,
            },
        );
        l.person(WorkRecord::ForkedFrom {
            item: id("r1"),
            from_attempt: a,
            at: ds[a as usize - 1],
            note: None,
        });
    }
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: 999,
        note: None,
    });
    let s = fold(&l.0);
    let why: Vec<_> = s.rejections.iter().map(|r| r.why).collect();
    // Three redos, then the fourth fork and the fifth claim's fork are refused.
    assert_eq!(why.iter().filter(|w| **w == Reject::NoRedosLeft).count(), 2);
    assert_eq!(s.budgets[&id("r1")].redos_left, 0);
}

#[test]
fn a_question_sends_the_item_to_needs_you_until_it_is_answered() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    let q = l.agent(
        "w1",
        WorkRecord::Question {
            item: id("r1"),
            attempt: 1,
            text: text("which client?"),
        },
    );
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::NeedsYou));
    l.person(WorkRecord::Answer {
        item: id("r1"),
        question: q,
        text: text("the rust one"),
        evidence: vec![],
    });
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Held));
    l.tracker(WorkRecord::Refused {
        item: Some(id("r1")),
        op: "work_post".into(),
        reason: RefusalReason::Budget,
    });
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::NeedsYou));
}
