use super::*;
use crate::codec::Codec;
use crate::content::Content;
use crate::work::{CloseWhy, DoneWhen, HumanKind, NewPrerequisite, Phase, Target, Then, Unclear};

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
        review: None,
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
        revert_check: None,
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
    assert_eq!(r1.paths.iter().map(|p| p.tries).sum::<u32>(), 2);
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
    // Two children drawn from the root, one fork on the item, 2.5 dollars of cost.
    assert_eq!((b.items_left, r1.redos), (9, 1));
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
    for n in 0..log.len() as u64 - 1 {
        // Everything after n becomes a fork at a decision that does not exist.
        let mut poisoned = log.clone();
        for (_, rec) in poisoned.iter_mut().filter(|(o, _)| *o > n) {
            rec.record = WorkRecord::ForkedFrom {
                item: id("r1"),
                from_attempt: 1,
                at: 999,
                note: None,
            };
            rec.actor = Actor::Person;
        }
        let want = fold(&log[..=n as usize]);
        // The poison is live: folded in full it leaves a rejection after n.
        assert!(fold(&poisoned).rejections.iter().any(|r| r.offset > n));
        let seek = Seek::build(&poisoned, 4);
        assert_eq!(seek.state_at(&poisoned, n), fold(&poisoned[..=n as usize]));
        assert_eq!(seek.state_at(&poisoned, n), want);
    }
    // Once the item has a path, a poisoned fork is refused for the missing decision.
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: 999,
        note: None,
    });
    let seek = Seek::build(&l.0, 2);
    assert_eq!(
        seek.state_at(&l.0, 4).rejections.last().unwrap().why,
        Reject::UnknownDecision
    );
    assert!(seek.state_at(&l.0, 3).rejections.is_empty());
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
        redos_per_item: 9,
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
                    && b.redos_per_item <= p.redos_per_item
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
    // The fold alone sends the root to Needs you; no `refused` record is needed.
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));

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
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
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
    assert!(!s.budgets.contains_key(&id("bug2")));
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
            revert_check: None,
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
fn human_inputs_follow_the_path_up_to_the_fork_plus_its_note() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1)); // 2
    let note_in = |l: &mut Log, attempt, t: &str| {
        l.person(WorkRecord::HumanInput {
            item: id("r1"),
            attempt,
            input: HumanKind::Note,
            text: text(t),
        })
    };
    let before = note_in(&mut l, Some(1), "early"); // 3
    let d = l.agent("w1", decision("r1", 1, vec![])); // 4
    l.agent("w1", decision("r1", 1, vec![])); // 5
    let after = note_in(
        &mut l,
        Some(1),
        "late, not the reviewer's note for the fork",
    ); // 6
    let whole = note_in(&mut l, None, "p0"); // 7
    // The reviewer's note is written after the decision it forks from.
    let steer = note_in(&mut l, Some(1), "take the writer"); // 8
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: d,
        note: Some(steer),
    });
    l.agent("w2", claimed("r1", 2));
    let own = note_in(&mut l, Some(2), "also this");
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!(
        s.human_inputs_on_path(&id("r1"), 1),
        vec![before, after, whole, steer]
    );
    // Inputs up to the fork decision, the note, and the path's own; not `after`.
    assert_eq!(
        s.human_inputs_on_path(&id("r1"), 2),
        vec![before, whole, steer, own]
    );
}

#[test]
fn redos_are_limited_per_item_and_a_fork_needs_a_real_decision() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    for a in 1..=4u32 {
        l.agent("w1", claimed("r1", a));
        let d = l.agent("w1", decision("r1", a, vec![]));
        l.agent(
            "w1",
            WorkRecord::Released {
                item: id("r1"),
                attempt: a,
                reason: None,
                cost_micros: 0,
            },
        );
        if a >= 2 {
            l.person(WorkRecord::AttemptClosed {
                item: id("r1"),
                attempt: a - 1,
            });
        }
        l.person(WorkRecord::ForkedFrom {
            item: id("r1"),
            from_attempt: a,
            at: d,
            note: None,
        });
    }
    let s = fold(&l.0);
    // Three redos, then the fourth fork is refused.
    let why: Vec<_> = s.rejections.iter().map(|r| r.why).collect();
    assert_eq!(why, vec![Reject::NoRedosLeft]);
    assert_eq!(s.items[&id("r1")].redos, 3);
    // The limit belongs to the item: its child still has all three.
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", proposed("c1", Some("r1"), "a", WorkKind::Split));
    l.agent("w1", proposed("c2", Some("r1"), "b", WorkKind::Split));
    for c in ["c1", "c2"] {
        l.agent("w2", claimed(c, 1));
        let d = l.agent("w2", decision(c, 1, vec![]));
        l.agent(
            "w2",
            WorkRecord::Released {
                item: id(c),
                attempt: 1,
                reason: None,
                cost_micros: 0,
            },
        );
        l.person(WorkRecord::ForkedFrom {
            item: id(c),
            from_attempt: 1,
            at: d,
            note: None,
        });
    }
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!((s.items[&id("c1")].redos, s.items[&id("c2")].redos), (1, 1));

    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: 999,
        note: None,
    });
    assert_eq!(fold(&l.0).rejections[0].why, Reject::UnknownDecision);
}

#[test]
fn a_path_gets_two_claims_then_needs_you() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    let release = |l: &mut Log, w: &str, cost| {
        l.agent(
            w,
            WorkRecord::Released {
                item: id("r1"),
                attempt: 1,
                reason: Some("lapsed".into()),
                cost_micros: cost,
            },
        );
    };
    l.agent("w1", claimed("r1", 1));
    release(&mut l, "w1", 1_000);
    // A retry stays on the same path.
    l.agent("w2", claimed("r1", 1));
    release(&mut l, "w2", 2_000);
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::NeedsYou));
    l.agent("w3", claimed("r1", 1));
    l.agent("w3", claimed("r1", 2));
    let s = fold(&l.0);
    assert_eq!(
        s.rejections.iter().map(|r| r.why).collect::<Vec<_>>(),
        vec![Reject::TooManyTries, Reject::TooManyTries]
    );
    let it = &s.items[&id("r1")];
    assert_eq!(
        (it.paths.len(), it.paths[0].tries, it.hold.is_none()),
        (1, 2, true)
    );
    // Every attempt draws its reported cost.
    assert_eq!(
        s.budgets[&id("r1")].cost_left_micros,
        DEFAULT_COST_MICROS - 3_000
    );
}

#[test]
fn a_finished_attempt_can_be_redone_until_the_item_is_closed() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    let d = l.agent("w1", decision("r1", 1, vec![]));
    l.agent("w1", done("r1", 1, 100));
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::InReview));
    // Request changes: redo from the decision while the PR is open.
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: d,
        note: None,
    });
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Ready));
    l.agent("w2", claimed("r1", 2));
    let d2 = l.agent("w2", decision("r1", 2, vec![]));
    l.agent(
        "w2",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 2,
            reason: None,
            cost_micros: 0,
        },
    );
    // Two attempts are open, so a third is refused until one is closed.
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 2,
        at: d2,
        note: None,
    });
    l.person(WorkRecord::AttemptClosed {
        item: id("r1"),
        attempt: 1,
    });
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 2,
        at: d2,
        note: None,
    });
    let s = fold(&l.0);
    assert_eq!(
        s.rejections.iter().map(|r| r.why).collect::<Vec<_>>(),
        vec![Reject::TooManyOpen]
    );
    assert_eq!(s.items[&id("r1")].redos, 2);
    // Merged closes the item for good.
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Merged,
    });
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 2,
        at: d2,
        note: None,
    });
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Done));
    assert_eq!(s.rejections.last().unwrap().why, Reject::Closed);
}

#[test]
fn only_the_holders_triage_and_answer_and_only_the_tracker_refuses() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.tracker(proposed("t1", Some("r1"), "triage", WorkKind::Triage));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    let triaged = |tier| WorkRecord::Triaged {
        item: id("r1"),
        tier,
        why: "w".into(),
        duplicate_of: None,
        unclear: None,
    };
    l.agent("w2", triaged(3)); // 3: holds nothing
    l.agent("w1", claimed("t1", 1));
    l.agent("w2", triaged(3)); // 5: w1 holds it
    l.agent("w1", triaged(0)); // 6
    l.agent("w3", claimed("r1", 1));
    let q = l.agent(
        "w3",
        WorkRecord::Question {
            item: id("r1"),
            attempt: 1,
            text: text("which?"),
            options: vec![],
            default: None,
            cost_micros: 0,
        },
    );
    l.agent("w3", proposed("q1", Some("r1"), "q", WorkKind::Question));
    let ans = || WorkRecord::Answer {
        item: id("r1"),
        question: q,
        text: text("this"),
        evidence: vec![],
        chosen: None,
        then: None,
    };
    l.agent("w4", ans()); // 10: not the holder
    l.tracker(ans()); // 11
    l.agent("w4", claimed("q1", 1));
    l.agent("w5", ans()); // 13
    let refused = || WorkRecord::Refused {
        item: Some(id("r1")),
        op: "work_post".into(),
        reason: RefusalReason::Budget,
    };
    l.agent("w3", refused()); // 14
    l.person(refused()); // 15
    let s = fold(&l.0);
    let rej: Vec<_> = s.rejections.iter().map(|r| (r.offset, r.why)).collect();
    assert_eq!(
        rej,
        vec![
            (3, Reject::NotHolder),
            (5, Reject::NotHolder),
            (10, Reject::NotHolder),
            (11, Reject::ActorNotAllowed),
            (13, Reject::NotHolder),
            (14, Reject::ActorNotAllowed),
            (15, Reject::ActorNotAllowed),
        ]
    );
    assert_eq!(s.items[&id("r1")].proposal.as_ref().unwrap().tier, 0);
    assert!(!s.items[&id("r1")].over_budget);
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    // The agent holding the question item answers.
    let mut l2 = Log(l.0.clone());
    l2.agent("w4", ans());
    assert!(fold(&l2.0).items[&id("r1")].open_question.is_none());
    // A person always can.
    let mut l3 = Log(l.0.clone());
    l3.person(ans());
    assert!(fold(&l3.0).items[&id("r1")].open_question.is_none());
}

#[test]
fn proposals_are_keyed_and_draw_no_budget_until_accepted() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", proposed("bug2", None, "found", WorkKind::Fix));
    l.agent("w1", proposed("bug2b", None, "found", WorkKind::Fix)); // retry, new id
    l.person(proposed("mine", None, "m", WorkKind::Fix));
    l.person(proposed("mine2", None, "m", WorkKind::Fix));
    let s = fold(&l.0);
    assert_eq!(
        s.rejections.iter().map(|r| r.why).collect::<Vec<_>>(),
        vec![Reject::Duplicate, Reject::Duplicate]
    );
    assert!(!s.budgets.contains_key(&id("bug2")));
    assert!(s.budgets.contains_key(&id("mine")));
    assert_eq!(s.stage(&id("bug2")), Some(Stage::Proposed));
    let mut l2 = Log(l.0.clone());
    l2.person(WorkRecord::Accepted {
        item: id("bug2"),
        tier: None,
    });
    let s = fold(&l2.0);
    assert!(s.budgets.contains_key(&id("bug2")));
    assert_eq!(s.stage(&id("bug2")), Some(Stage::Ready));
    // A proposal a person turns down is closed and rejected only if never accepted.
    let mut l3 = Log(l.0.clone());
    l3.person(WorkRecord::Rejected { item: id("bug2") });
    l3.person(WorkRecord::Rejected { item: id("mine") });
    let s = fold(&l3.0);
    assert_eq!(s.stage(&id("bug2")), Some(Stage::Closed));
    assert_eq!(s.rejections.last().unwrap().why, Reject::NotAProposal);
}

#[test]
fn closing_a_root_cancels_its_open_descendants() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", proposed("c1", Some("r1"), "a", WorkKind::Split));
    l.agent("w1", proposed("c2", Some("r1"), "b", WorkKind::Test));
    l.agent("w2", claimed("c2", 1));
    l.agent("w2", done("c2", 1, 0));
    l.agent("w3", claimed("c1", 1));
    l.agent("w3", proposed("g1", Some("c1"), "g", WorkKind::Augment));
    assert_eq!(fold(&l.0).stage(&id("g1")), Some(Stage::Ready));
    l.tracker(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Upstream,
    });
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Closed));
    // c2 finished before the close and keeps its own ending.
    assert_eq!(s.stage(&id("c2")), Some(Stage::Done));
    for c in ["c1", "g1"] {
        assert_eq!(s.stage(&id(c)), Some(Stage::Closed));
        assert_eq!(s.items[&id(c)].closed, Some(Closure::Cancelled));
    }
    assert!(s.items[&id("c1")].hold.is_none());
    assert!(s.items.keys().all(|k| s.stage(k) != Some(Stage::Ready)));
    // An agent cannot close anything.
    l.agent(
        "w1",
        WorkRecord::Closed {
            item: id("c1"),
            why: CloseWhy::Dismissed,
        },
    );
    assert_eq!(
        fold(&l.0).rejections.last().unwrap().why,
        Reject::ActorNotAllowed
    );
}

#[test]
fn people_nest_no_deeper_than_agents() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    let mut parent = "r1".to_string();
    for d in 1..=5 {
        let child = format!("p{d}");
        l.person(proposed(&child, Some(&parent), "k", WorkKind::Split));
        parent = child;
    }
    let s = fold(&l.0);
    assert!(s.items.contains_key(&id("p3")));
    assert!(!s.items.contains_key(&id("p4")));
    assert_eq!(s.rejections[0].why, Reject::TooDeep);
    // Raising the cap to the largest depth lets p4 in and cannot overflow the counter.
    l.person(WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 5,
        max_depth: u8::MAX,
        cost_left_micros: 1,
        redos_per_item: 3,
    });
    l.person(proposed("p4", Some("p3"), "k", WorkKind::Split));
    assert_eq!(fold(&l.0).items[&id("p4")].depth, 4);
}

#[test]
fn a_closed_attempt_is_never_claimable() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    let d = l.agent("w1", decision("r1", 1, vec![]));
    l.agent("w1", done("r1", 1, 0));
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: d,
        note: None,
    });
    l.agent("w2", claimed("r1", 2));
    l.agent(
        "w2",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 2,
            reason: None,
            cost_micros: 0,
        },
    );
    l.person(WorkRecord::AttemptClosed {
        item: id("r1"),
        attempt: 2,
    });
    // Attempt 1 is still open and finished, so the item is back in review.
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::InReview));
    l.agent("w3", claimed("r1", 2));
    assert_eq!(fold(&l.0).rejections.last().unwrap().why, Reject::NotReady);
    // With every attempt closed, nothing is claimable and a person must act.
    l.person(WorkRecord::AttemptClosed {
        item: id("r1"),
        attempt: 1,
    });
    l.agent("w3", claimed("r1", 1));
    l.agent("w3", claimed("r1", 3));
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::NoOpenAttempt));
    assert_eq!(s.items[&id("r1")].paths.len(), 2);
    assert!(s.items[&id("r1")].hold.is_none());
}

#[test]
fn a_refused_done_changes_nothing() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    let before = fold(&l.0);
    // A person's done for an attempt that does not exist.
    l.person(done("r1", 7, 5_000));
    let s = fold(&l.0);
    assert_eq!(s.rejections.last().unwrap().why, Reject::WrongAttempt);
    assert_eq!(s.items, before.items);
    assert_eq!(s.budgets, before.budgets);
    // A closed attempt cannot be finished either, and costs nothing.
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    l.person(WorkRecord::AttemptClosed {
        item: id("r1"),
        attempt: 1,
    });
    let before = fold(&l.0);
    l.person(done("r1", 1, 5_000));
    let s = fold(&l.0);
    assert_eq!(s.rejections.last().unwrap().why, Reject::Closed);
    assert_eq!(s.items, before.items);
    assert_eq!(s.budgets, before.budgets);
}

#[test]
fn agents_keys_do_not_collide_on_roots() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.tracker(imported("r2"));
    l.person(WorkRecord::Accepted {
        item: id("r2"),
        tier: None,
    });
    l.agent("w2", claimed("r2", 1));
    l.agent("w1", proposed("a", None, "found", WorkKind::Fix));
    l.agent("w2", proposed("b", None, "found", WorkKind::Fix));
    l.agent("w1", proposed("a2", None, "found", WorkKind::Fix));
    let s = fold(&l.0);
    assert!(s.items.contains_key(&id("a")) && s.items.contains_key(&id("b")));
    assert_eq!(
        s.rejections.iter().map(|r| r.why).collect::<Vec<_>>(),
        vec![Reject::Duplicate]
    );
}

#[test]
fn a_dropped_dependency_sends_the_dependent_to_needs_you() {
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
        item: id("c"),
        to: id("b"),
    });
    l.tracker(WorkRecord::Closed {
        item: id("b"),
        why: CloseWhy::Dismissed,
    });
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("a")), Some(Stage::NeedsYou));
    assert_eq!(s.needs_you(&id("a")), Some(NeedsYou::DependencyDropped));
    // A merged dependency frees its dependents instead.
    let mut m = Log(l.0[..l.0.len() - 1].to_vec());
    m.person(WorkRecord::Closed {
        item: id("b"),
        why: CloseWhy::Merged,
    });
    assert_eq!(fold(&m.0).stage(&id("a")), Some(Stage::Ready));
    // A person relinks, and the dependent is free.
    l.person(WorkRecord::Unlinked {
        item: id("a"),
        to: id("b"),
    });
    assert_eq!(fold(&l.0).stage(&id("a")), Some(Stage::Ready));
    assert_eq!(fold(&l.0).stage(&id("c")), Some(Stage::NeedsYou));
}

#[test]
fn a_release_must_name_the_running_attempt() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.tracker(WorkRecord::Released {
        item: id("r1"),
        attempt: 3,
        reason: None,
        cost_micros: 900,
    });
    let s = fold(&l.0);
    assert_eq!(s.rejections.last().unwrap().why, Reject::WrongAttempt);
    assert!(s.items[&id("r1")].hold.is_some());
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, DEFAULT_COST_MICROS);
    l.tracker(WorkRecord::Released {
        item: id("r1"),
        attempt: 1,
        reason: None,
        cost_micros: 900,
    });
    assert!(fold(&l.0).items[&id("r1")].hold.is_none());
}

#[test]
fn done_for_an_attempt_that_does_not_exist_changes_nothing() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    let before = fold(&l.0);
    l.person(done("r1", 7, 5_000));
    let s = fold(&l.0);
    assert_eq!(s.rejections.last().unwrap().why, Reject::WrongAttempt);
    assert_eq!(s.items, before.items);
    assert_eq!(s.budgets, before.budgets);
}

#[test]
fn upstream_closure_counts_as_done_or_dropped_by_its_reason() {
    let mut l = Log::default();
    for r in ["a", "b"] {
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
    let close = |l: &Log, why: &str| {
        let mut m = Log(l.0.clone());
        let line = format!(
            r#"{{"at_ms":1,"by":"tracker","actor":"tracker","kind":"closed","item":"b"{why}}}"#
        );
        m.0.push((
            m.0.len() as u64,
            WorkLogged::decode(line.as_bytes()).unwrap(),
        ));
        fold(&m.0)
    };
    let fixed = close(&l, r#","why":"completed""#);
    assert_eq!(fixed.stage(&id("b")), Some(Stage::Done));
    assert_eq!(fixed.stage(&id("a")), Some(Stage::Ready));
    // Not planned, a duplicate, a reason this version does not know, or none: dropped.
    for why in [r#","why":"upstream""#, r#","why":"wont_fix_v9""#, ""] {
        let s = close(&l, why);
        assert_eq!(s.stage(&id("a")), Some(Stage::NeedsYou), "{why}");
        assert_eq!(s.needs_you(&id("a")), Some(NeedsYou::DependencyDropped));
    }
}

#[test]
fn try_apply_leaves_no_trace_when_it_refuses() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.person(WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 5,
        max_depth: 3,
        cost_left_micros: 0,
        redos_per_item: 3,
    });
    let mut s = fold(&l.0);
    let before = s.clone();
    // No cost left: the claim is refused, and the root must not be flagged over budget.
    let claim = WorkLogged {
        at_ms: 0,
        by: key("w1"),
        actor: Actor::Agent,
        record: claimed("r1", 1),
    };
    assert_eq!(s.try_apply(3, &claim), Err(Reject::OverBudget));
    assert_eq!(s, before);
    // The same record through apply flags the root, as a replay of a logged one would.
    s.apply(3, &claim);
    assert!(s.items[&id("r1")].over_budget);
}

fn accepted_root(l: &mut Log, item: &str) {
    l.tracker(imported(item));
    l.person(WorkRecord::Accepted {
        item: id(item),
        tier: None,
    });
}

#[test]
fn an_upstream_edit_flags_the_item_until_it_is_triaged_again() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    l.tracker(WorkRecord::Updated {
        item: id("r1"),
        title: Some("new title".into()),
    });
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    let it = &s.items[&id("r1")];
    assert!(it.changed_upstream);
    assert_eq!(it.title, "new title");
    // It makes no work: the item is still Ready, with no children.
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    assert!(it.children.is_empty());

    l.person(WorkRecord::Triaged {
        item: id("r1"),
        tier: 1,
        why: "again".into(),
        duplicate_of: None,
        unclear: None,
    });
    assert!(!fold(&l.0).items[&id("r1")].changed_upstream);

    // An agent cannot write it, and a closed item takes none.
    l.agent(
        "w1",
        WorkRecord::Updated {
            item: id("r1"),
            title: None,
        },
    );
    assert_eq!(
        fold(&l.0).rejections.last().unwrap().why,
        Reject::ActorNotAllowed
    );
}

#[test]
fn leaving_scope_stops_a_free_item_and_waits_for_a_holder() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    accepted_root(&mut l, "r2");
    l.agent("w1", claimed("r2", 1));
    l.tracker(WorkRecord::LeftScope { item: id("r1") });
    l.tracker(WorkRecord::LeftScope { item: id("r2") });
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::LeftScope));
    // The holder keeps going.
    assert_eq!(s.stage(&id("r2")), Some(Stage::Held));
    // A claim on the withdrawn item is refused.
    l.agent("w2", claimed("r1", 1));
    assert_eq!(fold(&l.0).rejections.last().unwrap().why, Reject::NotReady);
    // Released, it stops.
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r2"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    let s = fold(&l.0);
    assert_eq!(s.needs_you(&id("r2")), Some(NeedsYou::LeftScope));
}

#[test]
fn reopening_returns_the_same_root_to_triage_with_its_budget() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    l.tracker(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Upstream,
    });
    let before = fold(&l.0).budgets[&id("r1")].clone();
    l.tracker(WorkRecord::Reopened { item: id("r1") });
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.stage(&id("r1")), Some(Stage::Triage));
    assert_eq!(s.budgets[&id("r1")], before);
    assert!(s.items[&id("r1")].changed_upstream);
    // An item that is open cannot be reopened.
    l.tracker(WorkRecord::Reopened { item: id("r1") });
    assert_eq!(fold(&l.0).rejections.last().unwrap().why, Reject::NotClosed);
}

#[test]
fn upstream_sync_records_are_about_open_roots() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    l.tracker(proposed("c1", Some("r1"), "t", WorkKind::Triage));
    l.tracker(WorkRecord::LeftScope { item: id("c1") });
    assert_eq!(fold(&l.0).rejections.last().unwrap().why, Reject::NotARoot);
    l.tracker(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Completed,
    });
    l.tracker(WorkRecord::LeftScope { item: id("r1") });
    assert_eq!(fold(&l.0).rejections.last().unwrap().why, Reject::Closed);
    assert_eq!(
        fold(&l.0)
            .imported()
            .map(|(s, u, i)| (s.to_owned(), u.to_owned(), i.clone()))
            .collect::<Vec<_>>(),
        vec![("github".to_owned(), "felix#r1".to_owned(), id("r1"))]
    );
}

/// A root accepted and held by w1 on attempt 1, with a budget big enough that the caps bite first.
fn held_root() -> Log {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.person(WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 500,
        max_depth: 3,
        cost_left_micros: 1_000_000,
        redos_per_item: 3,
    });
    l
}

fn step_rec(item: &str) -> WorkRecord {
    WorkRecord::Step {
        item: id(item),
        attempt: 1,
        command: "cargo test".into(),
        exit_code: 0,
        output: text("ok"),
    }
}

fn last_rejection(l: &Log) -> Option<Reject> {
    fold(&l.0).rejections.last().map(|r| r.why)
}

#[test]
fn an_attempt_is_capped_per_kind_of_record() {
    let mut l = held_root();
    for _ in 0..MAX_STEPS {
        l.agent("w1", step_rec("r1"));
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", step_rec("r1"));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));

    let mut l = held_root();
    for _ in 0..MAX_DECISIONS {
        l.agent("w1", decision("r1", 1, vec![]));
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", decision("r1", 1, vec![]));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));

    let mut l = held_root();
    let d = l.agent("w1", decision("r1", 1, vec![]));
    let edit = |d| WorkRecord::Edit {
        item: id("r1"),
        attempt: 1,
        file: "a.rs".into(),
        hunk: text("@@"),
        decision: d,
    };
    for _ in 0..MAX_EDITS {
        l.agent("w1", edit(d));
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", edit(d));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));

    let mut l = held_root();
    let progress = || WorkRecord::Progress {
        item: id("r1"),
        attempt: 1,
        note: text("working"),
        branch: None,
        pr: None,
    };
    for _ in 0..MAX_PROGRESS {
        l.agent("w1", progress());
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", progress());
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));
}

#[test]
fn a_refused_record_over_the_cap_leaves_the_state_alone() {
    let mut l = held_root();
    for _ in 0..MAX_STEPS {
        l.agent("w1", step_rec("r1"));
    }
    let before = fold(&l.0);
    l.agent("w1", step_rec("r1"));
    let after = fold(&l.0);
    assert_eq!(after.items, before.items);
}

#[test]
fn posts_and_context_are_capped_and_the_cap_is_logged_once_per_attempt() {
    let mut l = held_root();
    for i in 0..MAX_POSTS {
        l.agent(
            "w1",
            proposed(
                &format!("c{i}"),
                Some("r1"),
                &format!("k{i}"),
                WorkKind::Test,
            ),
        );
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", proposed("over", Some("r1"), "over", WorkKind::Test));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));
    // A proposal for out-of-scope work counts against the same attempt.
    l.agent("w1", proposed("lost", None, "found", WorkKind::Fix));
    assert!(!fold(&l.0).items.contains_key(&id("lost")));

    let refusal = WorkRecord::Refused {
        item: Some(id("r1")),
        op: "post".into(),
        reason: RefusalReason::Limit,
    };
    assert!(!fold(&l.0).items[&id("r1")].paths[0].capped);
    l.tracker(refusal);
    assert!(fold(&l.0).items[&id("r1")].paths[0].capped);

    let mut l = held_root();
    let ctx = |n: u32| WorkRecord::Context {
        item: id("r1"),
        entry: key(&format!("e{n}")),
        text: text("a note"),
    };
    for n in 0..MAX_CONTEXT {
        l.agent("w1", ctx(n));
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", ctx(MAX_CONTEXT));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));
}

#[test]
fn an_agent_proposes_out_of_scope_work_only_while_it_holds_something() {
    let mut l = Log::default();
    l.agent("w9", proposed("x", None, "found", WorkKind::Fix));
    assert_eq!(last_rejection(&l), Some(Reject::NoHeldParent));
}

#[test]
fn an_agent_writes_context_only_on_work_it_holds_or_a_child_it_holds() {
    let mut l = held_root();
    let ctx = |item: &str, e: &str| WorkRecord::Context {
        item: id(item),
        entry: key(e),
        text: text("repro: run it twice"),
    };
    l.agent("w1", ctx("r1", "own"));
    assert_eq!(last_rejection(&l), None);
    // Another agent holds nothing here.
    l.agent("w2", ctx("r1", "stranger"));
    assert_eq!(last_rejection(&l), Some(Reject::NotHolder));
    // An augment child held by w2 lets it add context to its target.
    l.agent("w1", proposed("aug", Some("r1"), "aug", WorkKind::Augment));
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    l.agent("w2", claimed("aug", 1));
    l.agent("w2", ctx("r1", "from-augment"));
    let s = fold(&l.0);
    assert_eq!(s.rejections.len(), 1);
    assert!(
        s.items[&id("r1")]
            .context
            .contains_key(&key("from-augment"))
    );
    assert_eq!(s.items[&id("aug")].paths[0].context_writes, 1);
}

#[test]
fn an_agent_links_only_inside_its_own_tree_and_under_an_item_it_holds() {
    let mut l = held_root();
    l.agent("w1", proposed("c1", Some("r1"), "a", WorkKind::Split));
    l.agent("w1", proposed("c2", Some("r1"), "b", WorkKind::Split));
    let link = |a: &str, b: &str| WorkRecord::Linked {
        item: id(a),
        to: id(b),
    };
    l.agent("w1", link("c2", "c1"));
    assert_eq!(last_rejection(&l), None);
    // A cycle is still a cycle.
    l.agent("w1", link("c1", "c2"));
    assert_eq!(last_rejection(&l), Some(Reject::Cycle));
    // Someone who holds nothing cannot order another agent's items.
    l.agent("w2", link("c1", "r1"));
    assert_eq!(last_rejection(&l), Some(Reject::NotHolder));
    // Another tree is out of reach even for the holder.
    l.tracker(imported("r2"));
    l.agent("w1", link("c1", "r2"));
    assert_eq!(last_rejection(&l), Some(Reject::CrossRoot));
    assert_eq!(fold(&l.0).edges.len(), 1);
}

#[test]
fn tiers_are_zero_to_three_and_a_person_can_move_an_item() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(4),
    });
    assert_eq!(last_rejection(&l), Some(Reject::BadTier));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(1),
    });
    l.person(WorkRecord::TierChanged {
        item: id("r1"),
        tier: 0,
    });
    assert_eq!(fold(&l.0).items[&id("r1")].tier, Some(0));
    l.person(WorkRecord::TierChanged {
        item: id("r1"),
        tier: 9,
    });
    assert_eq!(last_rejection(&l), Some(Reject::BadTier));
    l.agent(
        "w1",
        WorkRecord::TierChanged {
            item: id("r1"),
            tier: 3,
        },
    );
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    assert_eq!(fold(&l.0).items[&id("r1")].tier, Some(0));
}

#[test]
fn a_person_edits_a_proposal_before_accepting_it() {
    let mut l = held_root();
    l.agent("w1", proposed("p", None, "found", WorkKind::Fix));
    l.person(WorkRecord::Edited {
        item: id("p"),
        title: Some("the real title".into()),
        tier: Some(0),
    });
    let s = fold(&l.0);
    assert_eq!(s.items[&id("p")].title, "the real title");
    l.person(WorkRecord::Accepted {
        item: id("p"),
        tier: None,
    });
    assert_eq!(fold(&l.0).items[&id("p")].tier, Some(0));
    // Accepted: the queue owns it, and tier_changed is the way to move it.
    l.person(WorkRecord::Edited {
        item: id("p"),
        title: None,
        tier: Some(3),
    });
    assert_eq!(last_rejection(&l), Some(Reject::NotAProposal));
    l.agent(
        "w1",
        WorkRecord::Edited {
            item: id("p"),
            title: None,
            tier: Some(3),
        },
    );
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
}

#[test]
fn a_decision_needs_its_commit_and_an_empty_why_still_folds() {
    let mut l = held_root();
    let with = |why: &str, commit: &str| WorkRecord::Decision {
        item: id("r1"),
        attempt: 1,
        phase: Phase::Fix,
        so_far: "found the race".into(),
        question: "q".into(),
        options: vec![],
        chosen: "c".into(),
        why: why.into(),
        commit: commit.into(),
        evidence: vec![],
        revert_check: None,
    };
    l.agent("w1", with("the lock is held", ""));
    assert_eq!(last_rejection(&l), Some(Reject::MissingField));
    // Logs written before the tool layer asked for a why must fold as they did.
    l.agent("w1", with("", "abc"));
    assert_eq!(fold(&l.0).rejections.len(), 1);
    assert_eq!(fold(&l.0).items[&id("r1")].paths[0].decisions.len(), 1);
}

fn question_rec(item: &str) -> WorkRecord {
    WorkRecord::Question {
        item: id(item),
        attempt: 1,
        text: text("which?"),
        options: vec![],
        default: None,
        cost_micros: 0,
    }
}

fn answer_rec(question: u64) -> WorkRecord {
    WorkRecord::Answer {
        item: id("r1"),
        question,
        text: text("this"),
        evidence: vec![],
        chosen: None,
        then: None,
    }
}

#[test]
fn questions_answers_and_links_are_capped_per_attempt() {
    let mut l = held_root();
    for _ in 0..MAX_QUESTIONS {
        l.agent("w1", question_rec("r1"));
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", question_rec("r1"));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));

    // Each answer closes the question it answers, so ask and answer in turn.
    let mut l = held_root();
    l.agent("w1", proposed("q1", Some("r1"), "q", WorkKind::Question));
    l.agent("w2", claimed("q1", 1));
    for _ in 0..MAX_ANSWERS {
        let q = l.agent("w1", question_rec("r1"));
        l.agent("w2", answer_rec(q));
    }
    assert_eq!(last_rejection(&l), None);
    let q = l.agent("w1", question_rec("r1"));
    l.agent("w2", answer_rec(q));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));
    assert_eq!(fold(&l.0).items[&id("r1")].open_question, Some(q));

    // A replayed edge counts, so the fold stops a loop even if the tool layer misses it.
    let mut l = held_root();
    l.agent("w1", proposed("c1", Some("r1"), "a", WorkKind::Split));
    l.agent("w1", proposed("c2", Some("r1"), "b", WorkKind::Split));
    let link = || WorkRecord::Linked {
        item: id("c2"),
        to: id("c1"),
    };
    for _ in 0..MAX_LINKS {
        l.agent("w1", link());
    }
    assert_eq!(last_rejection(&l), None);
    l.agent("w1", link());
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));
}

#[test]
fn an_answer_is_taken_only_for_the_open_question() {
    let mut l = held_root();
    let old = l.agent("w1", question_rec("r1"));
    let open = l.agent("w1", question_rec("r1"));
    l.person(answer_rec(old));
    assert_eq!(last_rejection(&l), Some(Reject::NotOpenQuestion));
    assert_eq!(fold(&l.0).items[&id("r1")].open_question, Some(open));
    l.person(answer_rec(open));
    assert_eq!(fold(&l.0).rejections.len(), 1);
    assert_eq!(fold(&l.0).items[&id("r1")].open_question, None);
    // Nothing is open now, so the same answer again is refused.
    l.person(answer_rec(open));
    assert_eq!(last_rejection(&l), Some(Reject::NotOpenQuestion));
}

// ---- pull request status and stalled sources ----

use crate::work::{FailedCheck, PrChecks, PrPhase, PrReview, PrSnapshot};

fn snapshot(pr: &str, checks: PrChecks, review: PrReview) -> PrSnapshot {
    let failing = (checks == PrChecks::Red).then(|| FailedCheck {
        name: "lint".into(),
        url: "https://example.test/lint".into(),
    });
    PrSnapshot {
        pr: pr.into(),
        phase: PrPhase::Open,
        review,
        checks,
        failing,
    }
}

fn pr_status(item: &str, status: PrSnapshot) -> WorkRecord {
    WorkRecord::PrStatus {
        item: id(item),
        status,
    }
}

/// A root whose agent reported a pull request and finished.
fn with_pr() -> Log {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(1),
    });
    l.agent("w1", claimed("r1", 1));
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("opened"),
            branch: None,
            pr: Some("GetFelix/felix#9".into()),
        },
    );
    l.agent("w1", done("r1", 1, 0));
    l
}

#[test]
fn a_red_check_puts_the_item_in_needs_you_and_green_takes_it_out() {
    let mut l = with_pr();
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::InReview));
    l.tracker(pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed),
    ));
    let s = fold(&l.0);
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::CheckFailed));
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    assert_eq!(s.pr_waits(&id("r1")), None);
    l.tracker(pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Green, PrReview::Unreviewed),
    ));
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::InReview));
    assert_eq!(s.pr_waits(&id("r1")), Some(PrWaits::CiGreenNoReviewer));
    l.tracker(pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Green, PrReview::Approved),
    ));
    assert_eq!(
        fold(&l.0).pr_waits(&id("r1")),
        Some(PrWaits::ApprovedNotMerged)
    );
    assert!(fold(&l.0).rejections.is_empty());
}

#[test]
fn a_status_is_refused_when_it_is_not_for_the_reported_request_or_not_from_sync() {
    let mut l = with_pr();
    let good = snapshot("GetFelix/felix#9", PrChecks::Green, PrReview::Unreviewed);
    // Another request than the one the item reports.
    l.tracker(pr_status(
        "r1",
        snapshot("Evil/repo#1", PrChecks::Green, PrReview::Unreviewed),
    ));
    // An agent cannot write it, even for its own request.
    l.agent("w1", pr_status("r1", good.clone()));
    // Red without a failing check, and a failing check on green.
    let mut bare = snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed);
    bare.failing = None;
    l.tracker(pr_status("r1", bare));
    let mut stray = good.clone();
    stray.failing = Some(FailedCheck {
        name: "x".into(),
        url: "u".into(),
    });
    l.tracker(pr_status("r1", stray));
    // Text over the record limits.
    let mut long = snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed);
    long.failing.as_mut().unwrap().name = "n".repeat(201);
    l.tracker(pr_status("r1", long));
    let s = fold(&l.0);
    let why: Vec<Reject> = s.rejections.iter().map(|r| r.why).collect();
    assert_eq!(
        why,
        [
            Reject::WrongPr,
            Reject::ActorNotAllowed,
            Reject::MissingField,
            Reject::MissingField,
            Reject::TooLong
        ]
    );
    assert!(s.items[&id("r1")].pr_status.is_none());
}

#[test]
fn a_held_item_is_not_moved_by_a_red_check_and_a_new_request_clears_the_status() {
    let mut l = with_pr();
    l.0.pop(); // the agent has not finished: it still holds the item
    l.tracker(pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed),
    ));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.needs_you(&id("r1")), None);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Held));
    // A different request resets what was known about the old one.
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("new pr"),
            branch: None,
            pr: Some("GetFelix/felix#10".into()),
        },
    );
    assert!(fold(&l.0).items[&id("r1")].pr_status.is_none());
}

#[test]
fn only_the_tracker_reports_a_stalled_source() {
    let mut l = Log::default();
    let stall = |status| WorkRecord::SourceStalled {
        source: "github".into(),
        repo: "GetFelix/gone".into(),
        status,
    };
    l.agent("w1", stall(Some(404)));
    l.person(stall(Some(404)));
    assert_eq!(fold(&l.0).stalled_sources().count(), 0);
    l.tracker(stall(Some(404)));
    assert_eq!(
        fold(&l.0).stalled_sources().collect::<Vec<_>>(),
        [("github", "GetFelix/gone", 404)]
    );
    l.tracker(stall(None));
    assert_eq!(fold(&l.0).stalled_sources().count(), 0);
}

#[test]
fn a_log_from_before_pull_request_status_folds_the_same_and_old_snapshots_still_load() {
    // The fixed story has no new record kinds, so nothing about it may change.
    let s = fold(&story().0);
    assert!(s.items.values().all(|i| i.pr_status.is_none()));
    assert_eq!(s.stalled_sources().count(), 0);
    // A snapshot written before the fields existed reads back as the same state.
    let mut v = serde_json::to_value(&s).unwrap();
    v.as_object_mut().unwrap().remove("source_stalls");
    for item in v["items"].as_object_mut().unwrap().values_mut() {
        item.as_object_mut().unwrap().remove("pr_status");
    }
    assert_eq!(serde_json::from_value::<State>(v).unwrap(), s);
    // A pre-change record line decodes to the record it always did.
    let line = br#"{"kind":"progress","item":"r1","attempt":1,"note":{"text":"x"},"pr":"GetFelix/felix#9"}"#;
    assert!(matches!(
        WorkRecord::decode(line).unwrap(),
        WorkRecord::Progress { .. }
    ));
}

#[test]
fn a_merged_close_needs_a_merged_status_for_the_reported_request() {
    let mut l = with_pr();
    l.0.pop(); // still held, so the agent can swap its request
    let merge = || WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Merged,
    };
    // No status at all, then a merged status of some other request.
    l.tracker(merge());
    let mut other = snapshot("GetFelix/felix#9", PrChecks::Green, PrReview::Approved);
    other.phase = PrPhase::Merged;
    l.tracker(pr_status("r1", other.clone()));
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("swap"),
            branch: None,
            pr: Some("GetFelix/felix#10".into()),
        },
    );
    l.tracker(merge());
    let s = fold(&l.0);
    assert_eq!(
        s.rejections.iter().map(|r| r.why).collect::<Vec<_>>(),
        [Reject::NotMerged, Reject::NotMerged]
    );
    assert_eq!(s.items[&id("r1")].closed, None);
    // The merged status of the reported request does close it.
    other.pr = "GetFelix/felix#10".into();
    l.tracker(pr_status("r1", other));
    l.tracker(merge());
    assert_eq!(fold(&l.0).items[&id("r1")].closed, Some(Closure::Merged));
}

#[test]
fn a_state_with_a_stalled_source_encodes_and_the_stalls_are_capped() {
    let mut l = Log::default();
    let stall = |repo: &str, status| WorkRecord::SourceStalled {
        source: "github".into(),
        repo: repo.into(),
        status,
    };
    l.tracker(stall("GetFelix/gone", Some(404)));
    let s = fold(&l.0);
    let back: State = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.stalled_sources().count(), 1);

    for n in 0..MAX_STALLS + 3 {
        l.tracker(stall(&format!("o/r{n}"), Some(403)));
    }
    let s = fold(&l.0);
    assert_eq!(s.stalled_sources().count(), MAX_STALLS);
    assert_eq!(s.rejections.len(), 4);
    // A known repo can still be updated or cleared at the cap.
    l.tracker(stall("GetFelix/gone", Some(403)));
    l.tracker(stall("GetFelix/gone", None));
    assert_eq!(fold(&l.0).stalled_sources().count(), MAX_STALLS - 1);
}

#[test]
fn a_request_another_open_item_reports_cannot_be_reported_again() {
    let mut l = with_pr();
    l.0.pop(); // r1 still holds its item
    l.tracker(imported("r2"));
    l.person(WorkRecord::Accepted {
        item: id("r2"),
        tier: Some(1),
    });
    l.agent("w2", claimed("r2", 1));
    let progress = |item: &str, pr: &str| WorkRecord::Progress {
        item: id(item),
        attempt: 1,
        note: text("pr"),
        branch: None,
        pr: Some(pr.into()),
    };
    l.agent("w2", progress("r2", "GetFelix/felix#9"));
    let s = fold(&l.0);
    assert_eq!(
        s.rejections.iter().map(|r| r.why).collect::<Vec<_>>(),
        [Reject::PrInUse]
    );
    assert_eq!(s.items[&id("r2")].pr, None);
    // Its own request is fine, and r1 may repeat its own.
    l.agent("w2", progress("r2", "GetFelix/felix#11"));
    l.agent("w1", progress("r1", "GetFelix/felix#9"));
    assert_eq!(fold(&l.0).rejections.len(), 1);
}

fn needs(prereq: &str, new: bool, done_when: DoneWhen) -> WorkRecord {
    WorkRecord::Prerequisite {
        item: id("r1"),
        attempt: 1,
        prerequisite: id(prereq),
        key: key("need"),
        new: new.then(|| NewPrerequisite {
            target: Target {
                source: "github".into(),
                repo: "GetFelix/felix-gateway".into(),
            },
            title: "gateway needs a CAS call".into(),
            why: text("the client cannot swap atomically"),
            evidence: vec![],
        }),
        done_when,
        cost_micros: 0,
    }
}

fn accept(item: &str) -> WorkRecord {
    WorkRecord::Accepted {
        item: id(item),
        tier: None,
    }
}

fn merged(item: &str) -> WorkRecord {
    WorkRecord::Closed {
        item: id(item),
        why: CloseWhy::Merged,
    }
}

#[test]
fn a_prerequisite_blocks_the_holder_without_costing_a_try() {
    let mut l = held_root();
    l.agent("w1", needs("g1", true, DoneWhen::Merged));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    let r1 = &s.items[&id("r1")];
    assert!(r1.hold.is_none());
    assert_eq!(r1.paths[0].tries, 0);
    assert_eq!(r1.paths[0].posts, 1);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Blocked));
    // The proposal waits in the other repo's Triage, as an agent's proposal.
    let g1 = &s.items[&id("g1")];
    assert!(g1.parent.is_none() && g1.proposed_by_agent && !g1.accepted);
    assert_eq!(g1.target.as_ref().unwrap().repo, "GetFelix/felix-gateway");
    assert_eq!(s.stage(&id("g1")), Some(Stage::Proposed));
    assert!(s.edges.contains(&(id("r1"), id("g1"))));
    // The resumed claim is the first try again.
    l.person(accept("g1"));
    l.person(merged("g1"));
    l.agent("w1", claimed("r1", 1));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.items[&id("r1")].paths[0].tries, 1);
}

#[test]
fn a_finished_prerequisite_frees_the_holder() {
    let mut l = held_root();
    l.agent("w1", needs("g1", true, DoneWhen::Merged));
    l.person(accept("g1"));
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Blocked));
    l.person(merged("g1"));
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Ready));
}

#[test]
fn a_dropped_prerequisite_sends_the_holder_to_needs_you() {
    let mut l = held_root();
    l.agent("w1", needs("g1", true, DoneWhen::Merged));
    l.person(WorkRecord::Rejected { item: id("g1") });
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::DependencyDropped));
}

#[test]
fn a_link_that_waits_for_a_release_outlasts_the_merge() {
    let mut l = held_root();
    l.agent("w1", needs("g1", true, DoneWhen::Released));
    l.person(accept("g1"));
    let shipped = || WorkRecord::Shipped {
        item: id("g1"),
        release: "0.4.0".into(),
    };
    // Nothing ships before it is merged.
    l.person(shipped());
    assert_eq!(last_rejection(&l), Some(Reject::NotClosed));
    l.person(merged("g1"));
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Blocked));
    // An agent cannot say a release happened.
    l.agent("w1", shipped());
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Blocked));
    l.person(shipped());
    let s = fold(&l.0);
    assert_eq!(s.items[&id("g1")].release.as_deref(), Some("0.4.0"));
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
}

#[test]
fn a_link_that_waits_for_a_merge_does_not_wait_for_the_release() {
    let mut l = held_root();
    l.agent("w1", needs("g1", true, DoneWhen::Merged));
    l.person(accept("g1"));
    l.person(merged("g1"));
    let s = fold(&l.0);
    assert!(s.items[&id("g1")].release.is_none());
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
}

#[test]
fn a_prerequisite_cannot_close_a_cycle_and_a_refusal_leaves_the_claim() {
    let mut l = held_root();
    // Another tree whose item already waits on the held one.
    l.tracker(imported("r2"));
    l.person(accept("r2"));
    l.person(WorkRecord::Linked {
        item: id("r2"),
        to: id("r1"),
    });
    l.agent("w1", needs("r2", false, DoneWhen::Merged));
    assert_eq!(last_rejection(&l), Some(Reject::Cycle));
    l.agent("w1", needs("r1", false, DoneWhen::Merged));
    assert_eq!(last_rejection(&l), Some(Reject::Cycle));
    let s = fold(&l.0);
    assert!(s.items[&id("r1")].hold.is_some());
    assert_eq!(s.items[&id("r1")].paths[0].posts, 0);
    assert_eq!(s.edges.len(), 1);
}

#[test]
fn only_the_holder_names_a_prerequisite_and_a_new_one_needs_an_unused_key() {
    let mut l = held_root();
    l.agent("w2", needs("g1", true, DoneWhen::Merged));
    assert_eq!(last_rejection(&l), Some(Reject::NotHolder));
    l.person(needs("g1", true, DoneWhen::Merged));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    // The agent used the key for a proposal already.
    l.agent("w1", proposed("p1", None, "need", WorkKind::Fix));
    l.agent("w1", needs("g1", true, DoneWhen::Merged));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    assert!(fold(&l.0).items[&id("r1")].hold.is_some());
}

#[test]
fn prerequisites_count_toward_the_post_cap() {
    let mut l = held_root();
    for i in 0..MAX_POSTS {
        let (item, k) = (format!("p{i}"), format!("k{i}"));
        l.agent("w1", proposed(&item, None, &k, WorkKind::Fix));
    }
    assert!(fold(&l.0).rejections.is_empty());
    l.agent("w1", needs("g1", true, DoneWhen::Merged));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));
    assert!(!fold(&l.0).items.contains_key(&id("g1")));
}

#[test]
fn a_prerequisite_on_an_existing_item_counts_as_a_post_too() {
    let mut l = held_root();
    l.tracker(imported("r2"));
    l.agent("w1", needs("r2", false, DoneWhen::Released));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.items[&id("r1")].paths[0].posts, 1);
    assert!(s.release_edges.contains(&(id("r1"), id("r2"))));
}

fn with_cost(mut r: WorkRecord, cost: u64) -> WorkRecord {
    if let WorkRecord::Prerequisite { cost_micros, .. } = &mut r {
        *cost_micros = cost;
    }
    r
}

#[test]
fn a_reopened_prerequisite_needs_a_new_release() {
    let mut l = held_root();
    l.agent("w1", needs("g1", true, DoneWhen::Released));
    l.person(accept("g1"));
    l.person(merged("g1"));
    l.person(WorkRecord::Shipped {
        item: id("g1"),
        release: "0.4.0".into(),
    });
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Ready));
    l.tracker(WorkRecord::Reopened { item: id("g1") });
    assert!(fold(&l.0).items[&id("g1")].release.is_none());
    l.person(accept("g1"));
    l.person(merged("g1"));
    // Merged again, but the old release does not count.
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Blocked));
}

#[test]
fn a_prerequisite_charges_its_cost_and_a_chain_hits_the_budget() {
    let mut l = held_root();
    l.agent(
        "w1",
        with_cost(needs("g1", true, DoneWhen::Merged), 1_000_000),
    );
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, 1_000_000);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 0);
    l.person(accept("g1"));
    l.person(merged("g1"));
    l.agent("w1", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::OverBudget));
}

#[test]
fn prerequisite_evidence_cannot_cite_the_future() {
    let mut l = held_root();
    let mut r = needs("g1", true, DoneWhen::Merged);
    let next = l.0.len() as u64;
    if let WorkRecord::Prerequisite { new: Some(n), .. } = &mut r {
        n.evidence = vec![Evidence {
            stream: WORK_EVENTS.into(),
            offset: next + 5,
        }];
    }
    l.agent("w1", r);
    assert_eq!(last_rejection(&l), Some(Reject::EvidenceFromTheFuture));
    assert!(fold(&l.0).items[&id("r1")].hold.is_some());
}

#[test]
fn a_state_with_a_release_edge_encodes() {
    let mut l = held_root();
    l.agent("w1", needs("g1", true, DoneWhen::Released));
    let s = fold(&l.0);
    assert_eq!(s.release_edges.len(), 1);
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(serde_json::from_str::<State>(&json).unwrap(), s);
}

/// A root held by w1 that decided something and then asked `which?` with two options.
fn asked_with_options() -> (Log, u64, u64) {
    let mut l = held_root();
    let d = l.agent("w1", decision("r1", 1, vec![]));
    let q = l.agent(
        "w1",
        WorkRecord::Question {
            item: id("r1"),
            attempt: 1,
            text: text("which?"),
            options: vec!["read".into(), "write".into()],
            default: Some("read".into()),
            cost_micros: 0,
        },
    );
    (l, d, q)
}

fn answer_with(question: u64, chosen: Option<&str>, then: Option<Then>) -> WorkRecord {
    WorkRecord::Answer {
        item: id("r1"),
        question,
        text: text(if chosen.is_some() { "" } else { "neither" }),
        evidence: vec![],
        chosen: chosen.map(Into::into),
        then,
    }
}

fn resumes(s: &State) -> Vec<&Item> {
    s.items
        .values()
        .filter(|i| i.work == WorkKind::Resume)
        .collect()
}

#[test]
fn an_answer_makes_one_claimable_resume_item_and_a_second_makes_none() {
    let (mut l, d, q) = asked_with_options();
    let a = l.person(answer_with(q, Some("write"), Some(Then::Resume)));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    let r = resumes(&s);
    assert_eq!(r.len(), 1);
    // It carries the question, the answer and the asker's last so_far, by offset.
    let at = |name: &str| r[0].context[&key(name)];
    assert_eq!((at("question"), at("answer"), at("so_far")), (q, a, d));
    assert_eq!(s.stage(&r[0].id), Some(Stage::Ready));
    // The asker is out of Ready and no longer holds the item.
    assert_eq!(s.stage(&id("r1")), Some(Stage::Blocked));
    assert!(s.items[&id("r1")].hold.is_none());

    l.person(answer_with(q, Some("read"), Some(Then::Resume)));
    let s = fold(&l.0);
    assert_eq!(resumes(&s).len(), 1);
    assert_eq!(
        s.rejections.last().map(|r| r.why),
        Some(Reject::NotOpenQuestion)
    );
}

#[test]
fn the_asking_item_stays_out_of_ready_until_the_resume_work_is_done() {
    let (mut l, _, q) = asked_with_options();
    l.person(answer_with(q, None, Some(Then::Resume)));
    let rid = resumes(&fold(&l.0))[0].id.clone();
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Blocked));
    // Any agent may claim it, not only the asker.
    l.agent("w2", claimed(rid.as_str(), 1));
    let s = fold(&l.0);
    assert_eq!(s.stage(&rid), Some(Stage::Held));
    assert_eq!(s.stage(&id("r1")), Some(Stage::Blocked));
    l.agent("w2", done(rid.as_str(), 1, 0));
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Ready));
}

#[test]
fn an_answer_that_names_no_followup_makes_no_work() {
    // How every answer logged before resume work existed folds.
    let (mut l, _, q) = asked_with_options();
    l.person(answer_with(q, None, None));
    let s = fold(&l.0);
    assert!(resumes(&s).is_empty());
    assert_eq!(s.stage(&id("r1")), Some(Stage::Held));
}

#[test]
fn a_chosen_option_must_be_one_the_question_offered() {
    let (mut l, _, q) = asked_with_options();
    l.person(answer_with(q, Some("both"), Some(Then::Resume)));
    let s = fold(&l.0);
    assert_eq!(s.rejections.last().map(|r| r.why), Some(Reject::BadOption));
    assert!(resumes(&s).is_empty());
    assert!(s.items[&id("r1")].open_question.is_some());
}

#[test]
fn options_and_default_are_bounded() {
    let ask = |options: Vec<String>, default: Option<String>| WorkRecord::Question {
        item: id("r1"),
        attempt: 1,
        text: text("which?"),
        options,
        default,
        cost_micros: 0,
    };
    let opts = |n: usize| (0..n).map(|i| format!("o{i}")).collect::<Vec<_>>();
    for (rec, why) in [
        (ask(opts(MAX_ASK_OPTIONS + 1), None), Reject::BadOption),
        (ask(vec!["a".into(), "a".into()], None), Reject::BadOption),
        (ask(vec![String::new()], None), Reject::BadOption),
        (ask(opts(2), Some("zzz".into())), Reject::BadOption),
        (ask(vec![], Some("a".into())), Reject::BadOption),
        (
            ask(vec!["x".repeat(MAX_ASK_OPTION_LEN + 1)], None),
            Reject::TooLong,
        ),
    ] {
        let mut l = held_root();
        l.agent("w1", rec);
        assert_eq!(last_rejection(&l), Some(why));
        assert!(fold(&l.0).items[&id("r1")].open_question.is_none());
    }
    let mut l = held_root();
    l.agent("w1", ask(opts(MAX_ASK_OPTIONS), Some("o3".into())));
    assert!(fold(&l.0).rejections.is_empty());
}

fn triaged_unclear(item: &str) -> WorkRecord {
    WorkRecord::Triaged {
        item: id(item),
        tier: 2,
        why: "the issue names two repos".into(),
        duplicate_of: None,
        unclear: Some(Unclear::Repo),
    }
}

#[test]
fn an_unclear_triage_goes_to_a_person_as_a_question_not_to_ready() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    let t = l.person(triaged_unclear("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::Unclear));
    assert_eq!(s.items[&id("r1")].open_question, Some(t));
    // The answer is resume work, and the item is Ready again only after it is done.
    l.person(answer_with(t, None, Some(Then::Resume)));
    let s = fold(&l.0);
    assert_eq!(resumes(&s).len(), 1);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Blocked));
}

fn moved(source: &str, repo: &str) -> Then {
    Then::Prerequisite {
        target: Target {
            source: source.into(),
            repo: repo.into(),
        },
        title: "needs a CAS call".into(),
    }
}

#[test]
fn an_answer_that_names_another_repo_creates_a_prerequisite_there() {
    let (mut l, _, q) = asked_with_options();
    let a = l.person(answer_with(
        q,
        None,
        Some(moved("github", "GetFelix/felix-gateway")),
    ));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert!(resumes(&s).is_empty());
    let (_, p) = s.edges.iter().next().unwrap();
    let p = &s.items[p];
    assert_eq!(p.target.as_ref().unwrap().repo, "GetFelix/felix-gateway");
    assert_eq!(p.context[&key("prerequisite")], a);
    assert_eq!(s.stage(&p.id), Some(Stage::Proposed));
    assert_eq!(s.stage(&id("r1")), Some(Stage::Blocked));
    assert!(s.items[&id("r1")].hold.is_none());
}

#[test]
fn only_a_person_can_move_work_to_another_repo() {
    let mut l = held_root();
    let q = l.agent(
        "w1",
        WorkRecord::Question {
            item: id("r1"),
            attempt: 1,
            text: text("where?"),
            options: vec![],
            default: None,
            cost_micros: 0,
        },
    );
    l.agent("w1", proposed("q1", Some("r1"), "q", WorkKind::Question));
    l.agent("w2", claimed("q1", 1));
    l.agent(
        "w2",
        answer_with(q, None, Some(moved("github", "GetFelix/other"))),
    );
    let s = fold(&l.0);
    assert_eq!(
        s.rejections.last().map(|r| r.why),
        Some(Reject::ActorNotAllowed)
    );
    assert!(s.edges.is_empty());
    assert!(s.items[&id("r1")].open_question.is_some());
    // Its answer without a move is accepted, and starts resume work like a person's.
    l.agent("w2", answer_with(q, None, Some(Then::Resume)));
    assert_eq!(resumes(&fold(&l.0)).len(), 1);
}

#[test]
fn a_move_needs_a_target_and_a_title_within_bounds() {
    for (then, why) in [
        (moved("", "a/b"), Reject::MissingField),
        (moved("github", ""), Reject::MissingField),
        (
            moved("github", &"r".repeat(MAX_PR_REF + 1)),
            Reject::TooLong,
        ),
    ] {
        let (mut l, _, q) = asked_with_options();
        l.person(answer_with(q, None, Some(then)));
        assert_eq!(last_rejection(&l), Some(why));
        assert!(fold(&l.0).edges.is_empty());
    }
}

fn felix() -> Target {
    Target {
        source: "github".into(),
        repo: "GetFelix/felix".into(),
    }
}
fn imported_in_felix(item: &str) -> WorkRecord {
    WorkRecord::Imported {
        item: id(item),
        source: "github".into(),
        upstream_id: format!("GetFelix/felix#{item}"),
        title: format!("bug {item}"),
    }
}
fn rules(root: Option<&str>, t: Option<&str>, checks: Option<&[&str]>) -> WorkRecord {
    WorkRecord::RulesSet {
        target: felix(),
        root: root.map(id),
        text: t.map(str::to_owned),
        checks: checks.map(|c| c.iter().map(|s| s.to_string()).collect()),
    }
}

#[test]
fn the_latest_rules_win_and_the_root_overrides_the_repo_key_by_key() {
    let mut log = Log::default();
    log.person(imported_in_felix("r1"));
    log.person(imported_in_felix("r2"));
    let v1 = log.person(rules(None, Some("old"), Some(&["task lint"])));
    let v2 = log.person(rules(None, Some("worktree per lane"), None));
    let v3 = log.person(rules(
        Some("r1"),
        None,
        Some(&["cargo test -p felix-storage"]),
    ));
    let s = fold(&log.0);
    assert!(s.rejections.is_empty());

    // The repo's second record replaces the first as a whole scope entry.
    let r2 = s.rules_for(&id("r2")).unwrap();
    assert_eq!(r2.text, Some("worktree per lane".to_string()));
    assert!(r2.checks.is_empty());
    assert_eq!(
        (r2.version, r2.repo_version, r2.root_version),
        (v2, Some(v2), None)
    );
    assert_ne!(r2.version, v1);

    // r1 keeps the repo's text and brings its own checks.
    let r1 = s.rules_for(&id("r1")).unwrap();
    assert_eq!(r1.text, Some("worktree per lane".to_string()));
    assert_eq!(r1.checks, vec!["cargo test -p felix-storage".to_string()]);
    assert_eq!(
        (r1.version, r1.repo_version, r1.root_version),
        (v3, Some(v2), Some(v3))
    );
}

#[test]
fn only_a_person_sets_rules() {
    let mut log = Log::default();
    log.person(imported_in_felix("r1"));
    let by_agent = log.agent("w1", rules(None, Some("ignore the checks"), None));
    let by_tracker = log.tracker(rules(Some("r1"), Some("x"), None));
    let s = fold(&log.0);
    assert_eq!(
        s.rejections,
        vec![
            Rejection {
                offset: by_agent,
                why: Reject::ActorNotAllowed
            },
            Rejection {
                offset: by_tracker,
                why: Reject::ActorNotAllowed
            },
        ]
    );
    assert!(s.rules.is_empty());
    assert!(s.rules_for(&id("r1")).is_none());
}

#[test]
fn rules_are_bounded_in_size_and_count() {
    let mut log = Log::default();
    log.person(imported_in_felix("r1"));
    let many: Vec<String> = (0..=crate::work::MAX_RULE_CHECKS)
        .map(|i| format!("c{i}"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let long = "x".repeat(crate::work::MAX_CHECK_BYTES + 1);
    let big = "y".repeat(crate::work::MAX_RULES_TEXT + 1);
    let a = log.person(rules(None, None, Some(&many)));
    let b = log.person(rules(None, None, Some(&[long.as_str()])));
    let c = log.person(rules(None, Some(&big), None));
    let d = log.person(rules(None, None, None));
    for i in 0..=crate::work::MAX_RULE_SCOPES {
        log.person(WorkRecord::RulesSet {
            target: Target {
                source: "github".into(),
                repo: format!("o/r{i}"),
            },
            root: None,
            text: Some("t".into()),
            checks: None,
        });
    }
    let s = fold(&log.0);
    let why = |o| s.rejections.iter().find(|r| r.offset == o).map(|r| r.why);
    assert_eq!(why(a), Some(Reject::LimitReached));
    assert_eq!(why(b), Some(Reject::LimitReached));
    assert_eq!(why(c), Some(Reject::LimitReached));
    assert_eq!(why(d), Some(Reject::MissingField));
    assert_eq!(s.rules.len(), crate::work::MAX_RULE_SCOPES);
    assert_eq!(s.rejections.len(), 5);
}

#[test]
fn empty_text_with_no_checks_clears_a_scope_and_frees_its_slot() {
    let mut log = Log::default();
    log.person(imported_in_felix("r1"));
    log.person(rules(None, Some("repo text"), Some(&["task lint"])));
    log.person(rules(Some("r1"), Some("root text"), Some(&["cargo test"])));
    // Clearing the root puts the repo's rules back.
    log.person(rules(Some("r1"), Some(""), None));
    let s = fold(&log.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.rules.len(), 1);
    let r = s.rules_for(&id("r1")).unwrap();
    assert_eq!(r.text.as_deref(), Some("repo text"));
    assert_eq!(r.checks, vec!["task lint".to_string()]);
    assert_eq!(r.root_version, None);

    // A cleared scope no longer counts toward the cap.
    let mut log = Log::default();
    for i in 0..crate::work::MAX_RULE_SCOPES {
        log.person(WorkRecord::RulesSet {
            target: Target {
                source: "github".into(),
                repo: format!("o/r{i}"),
            },
            root: None,
            text: Some("t".into()),
            checks: None,
        });
    }
    let clear = |i: usize, text: &str| WorkRecord::RulesSet {
        target: Target {
            source: "github".into(),
            repo: format!("o/r{i}"),
        },
        root: None,
        text: Some(text.into()),
        checks: Some(vec![]),
    };
    log.person(clear(0, ""));
    let fresh = log.person(clear(9999, "new"));
    let s = fold(&log.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert!(s.rules.iter().any(|r| r.version == fresh));
    assert_eq!(s.rules.len(), crate::work::MAX_RULE_SCOPES);
}

#[test]
fn a_root_with_empty_checks_clears_the_repos_checks_but_keeps_its_text() {
    let mut log = Log::default();
    log.person(imported_in_felix("r1"));
    log.person(rules(None, Some("repo text"), Some(&["task lint"])));
    log.person(rules(Some("r1"), None, Some(&[])));
    let s = fold(&log.0);
    let r = s.rules_for(&id("r1")).unwrap();
    assert_eq!(r.text.as_deref(), Some("repo text"));
    assert!(r.checks.is_empty());
    assert_eq!(s.rules.len(), 2);
}

fn ask_on(item: &str, cost_micros: u64) -> WorkRecord {
    WorkRecord::Question {
        item: id(item),
        attempt: 1,
        text: text("which?"),
        options: vec![],
        default: None,
        cost_micros,
    }
}

fn answer_on(item: &str, question: u64) -> WorkRecord {
    WorkRecord::Answer {
        item: id(item),
        question,
        text: text("this"),
        evidence: vec![],
        chosen: None,
        then: Some(Then::Resume),
    }
}

#[test]
fn asking_charges_the_cost_so_far_because_the_answer_ends_the_hold() {
    let mut l = held_root();
    let q = l.agent("w1", ask_on("r1", 400_000));
    let s = fold(&l.0);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 600_000);
    assert_eq!(s.items[&id("r1")].cost_micros, 400_000);
    l.person(answer_on("r1", q));
    // The hold is gone, so a release could not report it: it was charged once, at the ask.
    let s = fold(&l.0);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 600_000);
    assert!(s.items[&id("r1")].hold.is_none());
}

#[test]
fn a_chain_of_asks_runs_the_root_out_of_budget() {
    let mut l = held_root();
    l.agent("w1", ask_on("r1", 600_000));
    let q = l.agent("w1", ask_on("r1", 400_000));
    assert_eq!(fold(&l.0).budgets[&id("r1")].cost_left_micros, 0);
    l.person(answer_on("r1", q));
    let rid = resumes(&fold(&l.0))[0].id.clone();
    l.agent("w2", claimed(rid.as_str(), 1));
    assert_eq!(last_rejection(&l), Some(Reject::OverBudget));
}

#[test]
fn resume_work_for_an_unaccepted_item_waits_for_its_acceptance() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    let t = l.person(triaged_unclear("r1"));
    l.person(answer_on("r1", t));
    let s = fold(&l.0);
    let rid = resumes(&s)[0].id.clone();
    assert!(s.rejections.is_empty());
    assert!(s.items.keys().all(|i| s.stage(i) != Some(Stage::Ready)));
    l.agent("w2", claimed(rid.as_str(), 1));
    assert_eq!(last_rejection(&l), Some(Reject::NotReady));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(1),
    });
    let s = fold(&l.0);
    assert_eq!(s.stage(&rid), Some(Stage::Ready));
    assert_eq!(s.items[&rid].tier, Some(1));
}

#[test]
fn chained_answers_stop_at_the_depth_cap() {
    let mut l = held_root();
    let mut at = "r1".to_string();
    let mut q = l.agent("w1", ask_on("r1", 0));
    // held_root allows three levels below the root.
    for n in 1..=3 {
        l.person(answer_on(&at, q));
        assert!(fold(&l.0).rejections.is_empty(), "level {n}");
        let r = resumes(&fold(&l.0))
            .into_iter()
            .find(|i| i.depth == n)
            .unwrap()
            .id
            .clone();
        l.agent("w2", claimed(r.as_str(), 1));
        at = r.to_string();
        q = l.agent("w2", ask_on(&at, 0));
    }
    l.person(answer_on(&at, q));
    assert_eq!(last_rejection(&l), Some(Reject::TooDeep));
    assert!(fold(&l.0).items[&id(&at)].open_question.is_some());
}

mod review;

#[test]
fn an_agent_cannot_replace_a_context_entry_it_did_not_write() {
    let mut l = held_root();
    let ctx = |e: &str, t: &str| WorkRecord::Context {
        item: id("r1"),
        entry: key(e),
        text: text(t),
    };
    // Ingest wrote `issue`; a person wrote `note`.
    l.tracker(ctx("issue", "upstream text"));
    l.person(ctx("note", "from a person"));
    let issue = l.0.len() as u64 - 2;
    l.agent("w1", ctx("issue", "forged"));
    assert_eq!(last_rejection(&l), Some(Reject::NotYourEntry));
    l.agent("w1", ctx("note", "forged"));
    assert_eq!(last_rejection(&l), Some(Reject::NotYourEntry));
    // Its own entry can be replaced, by itself only.
    l.agent("w1", ctx("mine", "one"));
    l.agent("w1", ctx("mine", "two"));
    let two = l.0.len() as u64 - 1;
    let s = fold(&l.0);
    assert_eq!(s.rejections.len(), 2);
    assert_eq!(s.items[&id("r1")].context[&key("issue")], issue);
    assert_eq!(s.items[&id("r1")].context[&key("mine")], two);
    // A person takes the entry over; the agent cannot take it back.
    l.person(ctx("mine", "person"));
    l.agent("w1", ctx("mine", "back"));
    assert_eq!(last_rejection(&l), Some(Reject::NotYourEntry));
}

#[test]
fn reported_costs_saturate_instead_of_overflowing() {
    let mut l = held_root();
    let ask = |cost_micros: u64| WorkRecord::Question {
        item: id("r1"),
        attempt: 1,
        text: text("which?"),
        options: vec![],
        default: None,
        cost_micros,
    };
    l.agent("w1", ask(u64::MAX));
    l.agent("w1", ask(1));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].cost_micros, u64::MAX);
    assert_eq!(s.budgets[&id("r1")].cost_left_micros, 0);
}
