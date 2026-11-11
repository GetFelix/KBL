use agents_core::codec::Codec;
use agents_core::content::Content;
use agents_core::names::{ItemId, Key};
use agents_core::work::*;

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
        },
        WorkRecord::Proposed {
            item: item("p1"),
            parent: None,
            key: key("found-1"),
            work: WorkKind::Fix,
            title: "a second bug".into(),
        },
        WorkRecord::Triaged {
            item: item("r1"),
            tier: 1,
            why: "data loss on restart".into(),
            duplicate_of: Some(item("r0")),
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
        },
        WorkRecord::Answer {
            item: item("r1"),
            question: 40,
            text: text("the rust one"),
            evidence: vec![Evidence {
                stream: WORK_EVENTS.into(),
                offset: 38,
            }],
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
        },
        WorkRecord::Prerequisite {
            item: item("r1"),
            attempt: 1,
            prerequisite: item("g2"),
            key: key("need-existing"),
            new: None,
            done_when: DoneWhen::Merged,
        },
        WorkRecord::Shipped {
            item: item("g1"),
            release: "0.4.0".into(),
        },
    ]
}

/// To change the format on purpose, regenerate with `WRITE_WIRE=1 cargo test -p agents-core --test work_wire`.
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
    // Every variant but `unknown`.
    assert_eq!(kinds.len(), 29);
}

#[test]
fn a_logged_record_carries_the_principal_and_role() {
    let line = WorkLogged {
        at_ms: 1_800_000_000_000,
        by: key("w3"),
        actor: Actor::Agent,
        record: WorkRecord::Done {
            item: item("r1"),
            attempt: 1,
            cost_micros: 0,
        },
    };
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
