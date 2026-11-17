use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;
use crate::work::read;

/// A log folded the way a page folds it: every record applied, the story's
/// fields kept for the ones the fold took.
struct Log {
    state: State,
    kept: BTreeMap<u64, Kept>,
    n: u64,
}

impl Log {
    fn new() -> Log {
        let mut l = Log {
            state: State::default(),
            kept: BTreeMap::new(),
            n: 0,
        };
        l.put("ingest", "tracker", json!({ "kind": "imported", "item": "r1", "source": "github", "upstream_id": "o/r#1", "title": "fix it" }));
        l.put(
            "gabriel",
            "person",
            json!({ "kind": "accepted", "item": "r1", "tier": 1 }),
        );
        l.put(
            "w1",
            "agent",
            json!({ "kind": "claimed", "item": "r1", "attempt": 1, "deadline_ms": 90000 }),
        );
        l
    }

    /// Appends one record and returns its offset.
    fn put(&mut self, by: &str, actor: &str, body: Value) -> u64 {
        let mut rec = json!({ "at_ms": self.n, "by": by, "actor": actor });
        rec.as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        let logged = read(rec.to_string().as_bytes()).unwrap();
        let at = self.n;
        self.n += 1;
        self.state.apply(at, &logged);
        if accepted(&self.state, at, &logged)
            && let Some(k) = Kept::of(&logged)
        {
            self.kept.insert(at, k);
        }
        at
    }

    fn agent(&mut self, body: Value) -> u64 {
        self.put("w1", "agent", body)
    }

    fn decide(&mut self, phase: &str, why: &str, evidence: &[u64]) -> u64 {
        self.agent(json!({
            "kind": "decision", "item": "r1", "attempt": 1, "phase": phase,
            "so_far": "found it", "question": "q", "chosen": "c", "why": why, "commit": "abc",
            "evidence": evidence.iter().map(|o| json!({ "stream": "work.events", "offset": o })).collect::<Vec<_>>(),
        }))
    }

    fn step(&mut self, command: &str, exit_code: i32) -> u64 {
        self.agent(
            json!({ "kind": "step", "item": "r1", "attempt": 1, "command": command,
            "exit_code": exit_code, "output": { "text": "out" } }),
        )
    }

    fn edit(&mut self, decision: u64) -> u64 {
        self.agent(
            json!({ "kind": "edit", "item": "r1", "attempt": 1, "file": "src/a.rs",
            "hunk": { "text": "@@ -1 +1 @@" }, "decision": decision }),
        )
    }

    fn story(&self) -> Option<Story> {
        story(&self.state, &"r1".parse().unwrap(), |o| self.kept.get(&o))
    }

    fn rules(&self) -> Vec<Rule> {
        self.story().unwrap().flags.iter().map(|f| f.rule).collect()
    }
}

#[test]
fn an_item_with_no_decision_has_no_story() {
    assert!(Log::new().story().is_none());
}

#[test]
fn decisions_are_grouped_by_phase_and_ordered_by_offset() {
    let mut l = Log::new();
    let fix = l.decide("fix", "patch the reader", &[]);
    let cause = l.decide("cause", "the cache is read early", &[]);
    let reproduce = l.decide("reproduce", "failing test", &[]);
    let fix2 = l.decide("fix", "and the writer", &[]);
    let s = l.story().unwrap();
    let phases: Vec<_> = s.groups.iter().map(|g| g.phase).collect();
    assert_eq!(phases, [Phase::Reproduce, Phase::Cause, Phase::Fix]);
    let fixes: Vec<_> = s.groups[2].decisions.iter().map(|d| d.offset).collect();
    assert_eq!(fixes, [fix, fix2]);
    assert_eq!(s.groups[0].decisions[0].offset, reproduce);
    assert_eq!(s.groups[1].decisions[0].offset, cause);
}

#[test]
fn a_second_attempt_and_a_persons_note_sit_in_one_ordered_story() {
    let mut l = Log::new();
    let d1 = l.decide("cause", "first guess", &[]);
    let note = l.put("gabriel", "person", json!({ "kind": "human_input", "item": "r1", "attempt": 1, "input": "review_comment", "text": { "text": "not the reader" } }));
    l.agent(json!({ "kind": "gave_up", "item": "r1", "attempt": 1, "reason": "stuck" }));
    l.put(
        "gabriel",
        "person",
        json!({ "kind": "forked_from", "item": "r1", "from_attempt": 1, "at": d1, "note": note }),
    );
    l.put(
        "w2",
        "agent",
        json!({ "kind": "claimed", "item": "r1", "attempt": 2, "deadline_ms": 90000 }),
    );
    let d2 = l.put("w2", "agent", json!({
        "kind": "decision", "item": "r1", "attempt": 2, "phase": "cause",
        "so_far": "again", "question": "q", "chosen": "writer", "why": "per the note", "commit": "def" }));
    let s = l.story().unwrap();
    let offsets: Vec<_> = s.groups[0]
        .decisions
        .iter()
        .map(|d| (d.offset, d.attempt, d.by.as_str()))
        .collect();
    assert_eq!(offsets, [(d1, 1, "w1"), (d2, 2, "w2")]);
    assert_eq!(s.people.len(), 1);
    assert_eq!(
        (s.people[0].offset, s.people[0].by.as_str()),
        (note, "gabriel")
    );
    assert_eq!(s.attempts[1].forked_from, Some((1, d1)));
}

#[test]
fn an_edit_and_its_evidence_hang_off_the_decision() {
    let mut l = Log::new();
    let run = l.step("cargo test", 1);
    let d = l.decide("fix", "guard the read", &[run]);
    let e = l.edit(d);
    let s = l.story().unwrap();
    let line = &s.groups[0].decisions[0];
    assert_eq!(line.edits.len(), 1);
    assert_eq!(
        (line.edits[0].offset, line.edits[0].file.as_str()),
        (e, "src/a.rs")
    );
    assert_eq!(
        line.evidence[0].found,
        Some(Found::Step {
            command: "cargo test".into(),
            exit_code: 1
        })
    );
}

#[test]
fn a_clean_attempt_has_no_flags() {
    let mut l = Log::new();
    let d = l.decide("fix", "guard the read", &[]);
    l.edit(d);
    let run = l.step("cargo test", 0);
    l.decide("verify", "the tests pass", &[run]);
    assert_eq!(l.rules(), []);
}

#[test]
fn claiming_pass_with_no_run_after_the_last_edit_is_flagged() {
    let mut l = Log::new();
    let before = l.step("cargo test", 0);
    let d = l.decide("fix", "guard the read", &[]);
    let e = l.edit(d);
    let v = l.decide("verify", "tests pass now", &[before]);
    let s = l.story().unwrap();
    let f = s
        .flags
        .iter()
        .find(|f| f.rule == Rule::ClaimsPassWithoutRun)
        .unwrap();
    assert_eq!((f.decision, f.offsets.clone()), (Some(v), vec![e]));
    // The run before the edit does not count, and the edit was never run.
    assert!(l.rules().contains(&Rule::EditsNeverRun));
}

#[test]
fn a_passing_run_after_the_edit_clears_the_claim() {
    let mut l = Log::new();
    let d = l.decide("fix", "guard the read", &[]);
    l.edit(d);
    l.step("cargo test", 0);
    l.decide("verify", "tests pass", &[]);
    assert_eq!(l.rules(), []);
}

#[test]
fn a_run_that_is_not_a_test_does_not_clear_the_claim() {
    let mut l = Log::new();
    let d = l.decide("fix", "guard the read", &[]);
    l.edit(d);
    l.step("cargo build", 0);
    l.decide("verify", "tests pass", &[]);
    assert!(l.rules().contains(&Rule::ClaimsPassWithoutRun));
}

#[test]
fn only_whole_test_runner_words_count_as_a_test_run() {
    for yes in [
        "cargo test cache",
        "cargo +nightly test -p x",
        "cargo nextest run",
        "npm test",
        "npm run test:unit",
        "pnpm test",
        "pytest -q",
        "python -m pytest",
        "python -m unittest",
        "go test ./...",
        "node --test test/*.ts",
        "cd core && cargo test",
        "task test",
    ] {
        assert!(is_test_run(yes), "{yes}");
    }
    for no in [
        "git checkout latest",
        "cargo build --contest",
        "grep attest log",
        "cat tests/data.json",
        "cargo build --tests",
        "echo test",
        "node script.js",
        "ls test",
    ] {
        assert!(!is_test_run(no), "{no}");
    }
}

#[test]
fn a_command_that_only_contains_test_does_not_clear_the_claim() {
    let mut l = Log::new();
    let d = l.decide("fix", "guard the read", &[]);
    l.edit(d);
    l.step("git checkout latest", 0);
    l.decide("verify", "tests pass", &[]);
    assert!(l.rules().contains(&Rule::ClaimsPassWithoutRun));
}

#[test]
fn citing_a_failed_step_for_a_pass_claim_is_flagged() {
    let mut l = Log::new();
    let run = l.step("cargo test", 101);
    l.decide("verify", "tests pass", &[run]);
    let s = l.story().unwrap();
    let f = s
        .flags
        .iter()
        .find(|f| f.rule == Rule::CitedStepFailed)
        .unwrap();
    assert_eq!(f.offsets, [run]);
    assert!(f.message.contains("exited 101"));
}

#[test]
fn citing_an_offset_that_is_no_recorded_step_is_flagged() {
    let mut l = Log::new();
    // Offset 0 is the import: a record of this log, but not a step, edit, decision or note.
    l.decide("cause", "see the import", &[0]);
    let s = l.story().unwrap();
    let f = s
        .flags
        .iter()
        .find(|f| f.rule == Rule::EvidenceNotRecorded)
        .unwrap();
    assert_eq!(f.offsets, [0]);
}

#[test]
fn an_unproven_revert_check_is_flagged_and_a_proven_one_is_marked() {
    let mut l = Log::new();
    let check = |failed| json!({ "test": "reader_guard", "failed_without_fix": failed, "passed_with_fix": true });
    let put = |l: &mut Log, failed| {
        l.agent(json!({
            "kind": "decision", "item": "r1", "attempt": 1, "phase": "verify",
            "so_far": "s", "question": "q", "chosen": "c", "why": "w", "commit": "abc",
            "revert_check": check(failed) }))
    };
    let bad = put(&mut l, false);
    let good = put(&mut l, true);
    let s = l.story().unwrap();
    let flagged: Vec<_> = s
        .flags
        .iter()
        .filter(|f| f.rule == Rule::RevertCheckNotProven)
        .map(|f| f.decision)
        .collect();
    assert_eq!(flagged, [Some(bad)]);
    let lines = &s.groups[0].decisions;
    assert_eq!((lines[0].proven, lines[1].proven), (false, true));
    assert_ne!(bad, good);
}

#[test]
fn a_fix_with_no_edit_is_flagged_and_an_edit_clears_it() {
    let mut l = Log::new();
    let d = l.decide("fix", "guard the read", &[]);
    assert_eq!(l.rules(), [Rule::FixWithoutEdit]);
    l.edit(d);
    assert!(!l.rules().contains(&Rule::FixWithoutEdit));
}

#[test]
fn edits_with_no_passing_run_after_them_are_flagged_per_attempt() {
    let mut l = Log::new();
    let d = l.decide("fix", "guard the read", &[]);
    let e = l.edit(d);
    l.step("cargo test", 1);
    let s = l.story().unwrap();
    let f = s
        .flags
        .iter()
        .find(|f| f.rule == Rule::EditsNeverRun)
        .unwrap();
    assert_eq!(
        (f.attempt, f.decision, f.offsets.clone()),
        (1, None, vec![e])
    );
}

#[test]
fn a_refused_record_is_not_kept() {
    let mut l = Log::new();
    // Attempt 2 is not held, so the fold refuses the decision.
    l.put(
        "w1",
        "agent",
        json!({
        "kind": "decision", "item": "r1", "attempt": 2, "phase": "cause",
        "so_far": "s", "question": "q", "chosen": "c", "why": "w", "commit": "abc" }),
    );
    assert!(l.kept.is_empty());
    assert!(l.story().is_none());
}

#[test]
fn a_steer_sits_in_the_story_as_a_note_on_its_attempt() {
    let mut l = Log::new();
    l.put(
        "gabriel",
        "person",
        json!({ "v": 12, "kind": "format_raised", "to": 12 }),
    );
    l.decide("cause", "first guess", &[]);
    let steer = l.put("gabriel", "person", json!({ "v": 12, "kind": "steered", "item": "r1", "attempt": 1, "text": { "text": "use the writer lock" } }));
    assert_eq!(l.state.rejections, vec![]);
    let s = l.story().unwrap();
    assert_eq!(s.people.len(), 1);
    let p = &s.people[0];
    assert_eq!(
        (p.offset, p.attempt, p.by.as_str()),
        (steer, Some(1), "gabriel")
    );
    assert_eq!(p.text, Content::Text("use the writer lock".into()));
}
