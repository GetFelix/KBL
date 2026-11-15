use kbl_core::codec::Codec;
use kbl_core::content::Content;
use kbl_core::names::{ItemId, Key};
use kbl_core::work::*;

fn item(s: &str) -> ItemId {
    ItemId::new(s).unwrap()
}
fn key(s: &str) -> Key {
    Key::new(s).unwrap()
}
fn text(s: &str) -> Content {
    Content::Text(s.into())
}

/// One record of every kind.
fn every_record() -> Vec<WorkRecord> {
    vec![
        WorkRecord::Imported {
            item: item("r1"),
            source: "github".into(),
            upstream_id: "felix#2204".into(),
            title: "reads miss a fresh write".into(),
        },
        WorkRecord::Proposed {
            item: item("c1"),
            parent: Some(item("r1")),
            key: key("split-a"),
            work: WorkKind::Split,
            title: "reader side".into(),
            review: None,
        },
        WorkRecord::Proposed {
            item: item("p1"),
            parent: None,
            key: key("found-1"),
            work: WorkKind::Fix,
            title: "a second bug".into(),
            review: None,
        },
        WorkRecord::Triaged {
            item: item("r1"),
            tier: 1,
            why: "data loss on restart".into(),
            duplicate_of: Some(item("r0")),
            unclear: None,
            depends_on: Vec::new(),
            split: Vec::new(),
        },
        WorkRecord::Accepted {
            item: item("r1"),
            tier: Some(1),
        },
        WorkRecord::Accepted {
            item: item("c1"),
            tier: None,
        },
        WorkRecord::Linked {
            item: item("c2"),
            to: item("c1"),
        },
        WorkRecord::Unlinked {
            item: item("c2"),
            to: item("c1"),
        },
        WorkRecord::Context {
            item: item("r1"),
            entry: key("repro"),
            text: text("run it twice"),
        },
        WorkRecord::Claimed {
            item: item("r1"),
            attempt: 1,
            deadline_ms: 1_800_000_090_000,
        },
        WorkRecord::Progress {
            item: item("r1"),
            attempt: 1,
            note: text("plan: fix the reader"),
            branch: Some("fix/r1".into()),
            pr: Some("GetFelix/felix#2210".into()),
        },
        WorkRecord::Done {
            item: item("r1"),
            attempt: 2,
            cost_micros: 1_250_000,
        },
        WorkRecord::Released {
            item: item("r1"),
            attempt: 1,
            reason: Some("claim lapsed".into()),
            cost_micros: 30_000,
        },
        WorkRecord::GaveUp {
            item: item("r1"),
            attempt: 1,
            reason: "cannot reproduce".into(),
            cost_micros: 400_000,
        },
        WorkRecord::Question {
            item: item("r1"),
            attempt: 1,
            text: text("which client?"),
            options: vec![],
            default: None,
            cost_micros: 0,
        },
        WorkRecord::Answer {
            item: item("r1"),
            question: 40,
            text: text("the rust one"),
            evidence: vec![Evidence {
                stream: WORK_EVENTS.into(),
                offset: 38,
            }],
            chosen: None,
            then: None,
        },
        WorkRecord::Decision {
            item: item("r1"),
            attempt: 1,
            phase: Phase::Cause,
            so_far: "the read races the fill".into(),
            question: "where does it go wrong".into(),
            options: vec!["reader".into(), "writer".into()],
            chosen: "reader".into(),
            why: "the writer holds the lock".into(),
            commit: "9f3c2ab".into(),
            evidence: vec![Evidence {
                stream: WORK_EVENTS.into(),
                offset: 12,
            }],
            revert_check: None,
        },
        WorkRecord::Step {
            item: item("r1"),
            attempt: 1,
            command: "cargo test -p felix-cache".into(),
            exit_code: 101,
            output: text("1 failed"),
        },
        WorkRecord::Edit {
            item: item("r1"),
            attempt: 1,
            file: "src/cache.rs".into(),
            hunk: text("@@ -10,2 +10,3 @@"),
            decision: 14,
        },
        WorkRecord::HumanInput {
            item: item("r1"),
            attempt: Some(1),
            input: HumanKind::ReviewComment,
            text: text("not in the reader"),
        },
        WorkRecord::HumanInput {
            item: item("r1"),
            attempt: None,
            input: HumanKind::PriorityChange,
            text: text("p0"),
        },
        WorkRecord::ForkedFrom {
            item: item("r1"),
            from_attempt: 1,
            at: 14,
            note: Some(22),
        },
        WorkRecord::Closed {
            item: item("r1"),
            why: CloseWhy::Merged,
        },
        WorkRecord::Closed {
            item: item("r2"),
            why: CloseWhy::Completed,
        },
        WorkRecord::Rejected { item: item("p1") },
        WorkRecord::Updated {
            item: item("r1"),
            title: Some("reads miss a fresh write after restart".into()),
        },
        WorkRecord::Updated {
            item: item("r1"),
            title: None,
        },
        WorkRecord::LeftScope { item: item("r1") },
        WorkRecord::Reopened { item: item("r1") },
        WorkRecord::AttemptClosed {
            item: item("r1"),
            attempt: 1,
        },
        WorkRecord::BudgetSet {
            root: item("r1"),
            items_left: 20,
            max_depth: 3,
            cost_left_micros: 30_000_000,
            redos_per_item: 3,
        },
        WorkRecord::RulesSet {
            target: Target {
                source: "github".into(),
                repo: "GetFelix/felix".into(),
            },
            root: None,
            text: Some("one worktree per lane".into()),
            checks: Some(vec!["task lint".into()]),
        },
        WorkRecord::RulesSet {
            target: Target {
                source: "github".into(),
                repo: "GetFelix/felix".into(),
            },
            root: Some(item("r1")),
            text: None,
            checks: Some(vec![]),
        },
        WorkRecord::PrStatus {
            item: item("r1"),
            status: PrSnapshot {
                pr: "GetFelix/felix#9".into(),
                phase: PrPhase::Open,
                review: PrReview::Unreviewed,
                checks: PrChecks::Red,
                failing: Some(FailedCheck {
                    name: "lint".into(),
                    url: "https://github.com/GetFelix/felix/actions/runs/1".into(),
                }),
            },
        },
        WorkRecord::SourceStalled {
            source: "github".into(),
            repo: "GetFelix/gone".into(),
            status: Some(404),
        },
        WorkRecord::Refused {
            item: Some(item("r1")),
            op: "work_post".into(),
            reason: RefusalReason::Budget,
        },
        WorkRecord::Refused {
            item: None,
            op: "work_post".into(),
            reason: RefusalReason::Limit,
        },
        WorkRecord::Prerequisite {
            item: item("r1"),
            attempt: 1,
            prerequisite: item("g1"),
            key: key("need-cas"),
            new: Some(NewPrerequisite {
                target: Target {
                    source: "github".into(),
                    repo: "GetFelix/felix-gateway".into(),
                },
                title: "gateway needs a CAS call".into(),
                why: text("the client cannot swap atomically"),
                evidence: vec![Evidence {
                    stream: WORK_EVENTS.into(),
                    offset: 12,
                }],
            }),
            done_when: DoneWhen::Released,
            cost_micros: 25_000,
            triage: None,
        },
        WorkRecord::Prerequisite {
            item: item("r1"),
            attempt: 1,
            prerequisite: item("g2"),
            key: key("need-existing"),
            new: None,
            done_when: DoneWhen::Merged,
            cost_micros: 0,
            triage: None,
        },
        WorkRecord::Shipped {
            item: item("g1"),
            release: "0.4.0".into(),
        },
        WorkRecord::Triaged {
            item: item("r1"),
            tier: 2,
            why: "which repo does this belong in".into(),
            duplicate_of: None,
            unclear: Some(Unclear::Repo),
            depends_on: Vec::new(),
            split: Vec::new(),
        },
        WorkRecord::Question {
            item: item("r1"),
            attempt: 1,
            text: text("which client?"),
            options: vec!["rust".into(), "python".into()],
            default: Some("rust".into()),
            cost_micros: 25_000,
        },
        WorkRecord::Answer {
            item: item("r1"),
            question: 41,
            text: text(""),
            evidence: vec![],
            chosen: Some("rust".into()),
            then: Some(Then::Resume),
        },
        WorkRecord::Answer {
            item: item("r1"),
            question: 42,
            text: text("this is a gateway bug"),
            evidence: vec![],
            chosen: None,
            then: Some(Then::Prerequisite {
                target: Target {
                    source: "github".into(),
                    repo: "GetFelix/felix-gateway".into(),
                },
                title: "gateway needs a CAS call".into(),
            }),
        },
        // A request reviewed end to end, on its own root so the records above stay as they were.
        WorkRecord::Imported {
            item: item("rv"),
            source: "github".into(),
            upstream_id: "GetFelix/felix#9".into(),
            title: "stale read".into(),
        },
        WorkRecord::Accepted {
            item: item("rv"),
            tier: Some(1),
        },
        WorkRecord::Claimed {
            item: item("rv"),
            attempt: 1,
            deadline_ms: 1_800_000_090_000,
        },
        WorkRecord::Decision {
            item: item("rv"),
            attempt: 1,
            phase: Phase::Verify,
            so_far: "fix in, test added".into(),
            question: String::new(),
            options: vec![],
            chosen: String::new(),
            why: "the test fails without the fix".into(),
            commit: "9f3c2ab".into(),
            evidence: vec![],
            revert_check: Some(RevertCheck {
                test: "reads_see_a_fresh_write".into(),
                failed_without_fix: true,
                passed_with_fix: true,
            }),
        },
        WorkRecord::Progress {
            item: item("rv"),
            attempt: 1,
            note: text("fixed"),
            branch: Some("fix/rv".into()),
            pr: Some("GetFelix/felix#9".into()),
        },
        WorkRecord::Done {
            item: item("rv"),
            attempt: 1,
            cost_micros: 0,
        },
        WorkRecord::PrStatus {
            item: item("rv"),
            status: PrSnapshot {
                pr: "GetFelix/felix#9".into(),
                phase: PrPhase::Open,
                review: PrReview::Unreviewed,
                checks: PrChecks::Green,
                failing: None,
            },
        },
        WorkRecord::Proposed {
            item: item("v1"),
            parent: Some(item("rv")),
            key: key("review-first"),
            work: WorkKind::Review,
            title: "First-pass review: GetFelix/felix#9".into(),
            review: Some(ReviewSpec {
                pass: ReviewPass::First,
                tier: "fast".into(),
                pr: "GetFelix/felix#9".into(),
            }),
        },
        WorkRecord::Proposed {
            item: item("v2"),
            parent: Some(item("rv")),
            key: key("review-final"),
            work: WorkKind::Review,
            title: "Final review: GetFelix/felix#9".into(),
            review: Some(ReviewSpec {
                pass: ReviewPass::Final,
                tier: "deep".into(),
                pr: "GetFelix/felix#9".into(),
            }),
        },
        WorkRecord::Claimed {
            item: item("v1"),
            attempt: 1,
            deadline_ms: 1_800_000_090_000,
        },
        WorkRecord::Finding {
            item: item("v1"),
            attempt: 1,
            severity: Severity::Medium,
            file: "src/read.rs".into(),
            line: 120,
            title: "other readers skip the check".into(),
            scenario: "a second reader path never looks at the cache epoch".into(),
            verdict: FindingVerdict::Defer,
        },
        WorkRecord::Done {
            item: item("v1"),
            attempt: 1,
            cost_micros: 0,
        },
        WorkRecord::Claimed {
            item: item("v2"),
            attempt: 1,
            deadline_ms: 1_800_000_090_000,
        },
        WorkRecord::Finding {
            item: item("v2"),
            attempt: 1,
            severity: Severity::High,
            file: "src/read.rs".into(),
            line: 88,
            title: "stale read after restart".into(),
            scenario: "restart between fill and read returns the old value".into(),
            verdict: FindingVerdict::Fix,
        },
        WorkRecord::ReviewVerdict {
            item: item("v2"),
            attempt: 1,
            verdict: MergeVerdict::Block,
            why: "a fix is still open".into(),
        },
    ]
}

/// To change the format on purpose, regenerate with `WRITE_WIRE=1 cargo test -p kbl-core --test work_wire`.
#[test]
fn wire_form_matches_the_golden_file() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/work_wire.jsonl");
    let now: String = every_record()
        .iter()
        .map(|r| String::from_utf8(r.encode()).unwrap() + "\n")
        .collect();
    if std::env::var_os("WRITE_WIRE").is_some() {
        std::fs::write(path, &now).unwrap();
    }
    assert_eq!(now, std::fs::read_to_string(path).unwrap());
}

#[test]
fn every_kind_round_trips_and_is_covered() {
    let all = every_record();
    for r in &all {
        assert_eq!(&WorkRecord::decode(&r.encode()).unwrap(), r);
        let line = String::from_utf8(r.encode()).unwrap();
        assert!(line.starts_with(&format!(r#"{{"kind":"{}""#, r.kind())));
    }
    let mut kinds: Vec<_> = all.iter().map(|r| r.kind()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    // Every first-format kind but `tier_changed` and `edited`, which
    // `tests/format.rs` covers along with `format_raised`.
    assert_eq!(kinds.len(), 34);
}

/// A triage answer from before dependencies and splits has neither field and
/// reads back with none; one with them round trips.
#[test]
fn a_triage_answer_carries_dependencies_and_a_split_only_when_it_has_them() {
    let old = br#"{"kind":"triaged","item":"r1","tier":1,"why":"w"}"#;
    let WorkRecord::Triaged {
        depends_on, split, ..
    } = WorkRecord::decode(old).unwrap()
    else {
        panic!("not a triaged record");
    };
    assert!(depends_on.is_empty() && split.is_empty());
    let with = WorkRecord::Triaged {
        item: item("r1"),
        tier: 1,
        why: "w".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![item("r0")],
        split: vec!["reader".into(), "writer".into()],
    };
    let line = String::from_utf8(with.encode()).unwrap();
    assert!(
        line.contains(r#""depends_on":["r0"]"#) && line.contains(r#""split":["reader","writer"]"#)
    );
    assert_eq!(WorkRecord::decode(line.as_bytes()).unwrap(), with);
}

#[test]
fn a_logged_record_carries_the_principal_and_role() {
    let line = WorkLogged::new(
        1_800_000_000_000,
        key("w3"),
        Actor::Agent,
        WorkRecord::Done {
            item: item("r1"),
            attempt: 1,
            cost_micros: 0,
        },
    );
    assert_eq!(
        String::from_utf8(line.encode()).unwrap(),
        r#"{"at_ms":1800000000000,"by":"w3","actor":"agent","kind":"done","item":"r1","attempt":1,"cost_micros":0}"#
    );
    assert_eq!(WorkLogged::decode(&line.encode()).unwrap(), line);
}

#[test]
fn keys_have_the_documented_shapes() {
    let (r1, c1) = (item("r1"), item("c1"));
    assert_eq!(item_key(&r1).unwrap().as_str(), "i-r1");
    assert_eq!(edge_key(&c1, &r1).unwrap().as_str(), "e-c1-r1");
    assert_eq!(
        context_key(&r1, &key("repro")).unwrap().as_str(),
        "c-r1-repro"
    );
    assert_eq!(budget_key(&r1).unwrap().as_str(), "b-r1");
    assert_eq!(
        child_key(&r1, &key("split-a")).unwrap().as_str(),
        "k-r1-split-a"
    );
    assert_eq!(
        upstream_key("github", "felix-2204").unwrap().as_str(),
        "u-github-felix-2204"
    );
    assert_eq!(ready_stream(0), "work.ready.p0");
    // Two long ids do not fit one key; the caller gets an error, not a truncation.
    let long = item(&"a".repeat(40));
    assert!(edge_key(&long, &long).is_err());
}
