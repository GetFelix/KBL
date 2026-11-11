use super::*;
use crate::codec::Codec;
use crate::content::Content;
use crate::work::{CloseWhy, HumanKind, Phase};

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
        },
    );
    l.agent("w3", proposed("q1", Some("r1"), "q", WorkKind::Question));
    let ans = || WorkRecord::Answer {
        item: id("r1"),
        question: q,
        text: text("this"),
        evidence: vec![],
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
    m.tracker(WorkRecord::Closed {
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
