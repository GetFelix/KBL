use std::collections::BTreeSet;

use agents_core::artifact::ArtifactId;
use agents_core::codec::Codec;
use agents_core::content::Content;
use agents_core::names::{AgentName, Key, TaskId};
use agents_core::record::{AgentSpec, Record};
use agents_core::stored::{Presence, TaskResult, lease_holder, lease_value};
use agents_core::task::Task;

fn agent(s: &str) -> AgentName {
    AgentName::new(s).unwrap()
}
fn task(s: &str) -> TaskId {
    TaskId::new(s).unwrap()
}
fn key(s: &str) -> Key {
    Key::new(s).unwrap()
}
fn art(s: &str) -> ArtifactId {
    ArtifactId::of(s.as_bytes())
}

/// One record of every kind.
fn every_record() -> Vec<Record> {
    vec![
        Record::RunStarted {
            goal: "Review PR #482".into(),
            budget_micros: 10_000_000,
            agents: vec![
                AgentSpec {
                    name: agent("planner"),
                    role: "planner".into(),
                },
                AgentSpec {
                    name: agent("w1"),
                    role: "reviewer".into(),
                },
            ],
        },
        Record::RunEnded {
            outcome: "review posted".into(),
        },
        Record::AgentUp {
            agent: agent("w1"),
            role: "reviewer".into(),
            model: "small".into(),
            presence: 4,
        },
        Record::AgentGone {
            agent: agent("w6"),
            presence: 9,
        },
        Record::TaskPosted {
            task: task("t-01"),
            parent: task("plan-1"),
            title: "src/main.rs".into(),
            offset: 0,
        },
        Record::TaskClaimed {
            task: task("t-01"),
            attempt: 1,
            deadline_ms: 1_800_000_090_000,
        },
        Record::TaskExtended {
            task: task("t-01"),
            attempt: 1,
            deadline_ms: 1_800_000_120_000,
        },
        Record::TaskReleased {
            task: task("t-01"),
            attempt: 1,
            reason: Some("binary file".into()),
        },
        Record::TaskReleased {
            task: task("t-02"),
            attempt: 2,
            reason: None,
        },
        Record::TaskDone {
            task: task("t-01"),
            attempt: 1,
            result: art("done"),
        },
        Record::TaskDuplicate {
            task: task("t-01"),
            attempt: 2,
            result: art("late"),
        },
        Record::TaskDead {
            task: task("t-31"),
            attempt: 2,
            reason: "given up after 2 attempts".into(),
        },
        Record::BoardPut {
            key: key("w3-notes"),
            version: 12,
            value: Content::Text("uses a fixed window".into()),
        },
        Record::BoardConflict {
            key: key("shared-api"),
            found: 14,
            value: Content::Artifact(art("api notes")),
        },
        Record::LeaseTaken {
            key: key("shared-limiter-clock"),
            version: 20,
            ttl_ms: 30_000,
        },
        Record::LeaseRenewed {
            key: key("shared-limiter-clock"),
            version: 23,
            ttl_ms: 30_000,
        },
        Record::LeaseReleased {
            key: key("shared-api"),
            version: 23,
        },
        Record::LeaseLost {
            key: key("shared-limiter-clock"),
            version: 23,
            holder: agent("w6"),
        },
        Record::Message {
            to: agent("w5"),
            corr: "m-12".into(),
            offset: 8,
        },
        Record::ModelStarted {
            agent: agent("planner"),
            task: task("plan-1"),
            reserve_micros: 600_000,
        },
        Record::ModelCall {
            agent: agent("planner"),
            task: task("plan-1"),
            prompt: art("prompt"),
            response: art("response"),
            tokens: 18_400,
            cost_micros: 420_000,
        },
        Record::Refused {
            op: "board_put".into(),
            resource: "agents.board.r-7f3a".into(),
            key: Some("plan-review".into()),
            error: "Forbidden: no grant on plan-*".into(),
        },
        Record::Refused {
            op: "publish".into(),
            resource: "agents.tasks.r-7f3a".into(),
            key: None,
            error: "Forbidden".into(),
        },
    ]
}

#[test]
fn every_record_round_trips() {
    for record in every_record() {
        let bytes = record.encode();
        assert_eq!(
            Record::decode(&bytes).unwrap(),
            record,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
    }
}

#[test]
fn the_kind_field_is_kind() {
    for record in every_record() {
        let json: serde_json::Value = serde_json::from_slice(&record.encode()).unwrap();
        assert_eq!(json["kind"], record.kind());
    }
}

/// The kinds in the design's run log table are exactly the kinds that exist.
#[test]
fn the_design_table_and_the_code_agree() {
    let design =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/design.md")).unwrap();
    let table = design
        .split("### Run log records")
        .nth(1)
        .and_then(|rest| rest.split("## The Felix mapping").next())
        .expect("the run log records section");
    let mut documented = BTreeSet::new();
    for line in table.lines().filter(|l| l.starts_with("| `")) {
        let first_column = line.split('|').nth(1).unwrap();
        for part in first_column.split('`').skip(1).step_by(2) {
            documented.insert(part.to_string());
        }
    }
    let implemented: BTreeSet<String> = every_record()
        .iter()
        .map(|r| r.kind().to_string())
        .collect();
    assert_eq!(documented, implemented);
}

#[test]
fn the_wire_form_is_stable() {
    let r = Record::TaskClaimed {
        task: task("t-01"),
        attempt: 1,
        deadline_ms: 90_000,
    };
    assert_eq!(
        String::from_utf8(r.encode()).unwrap(),
        r#"{"kind":"task_claimed","task":"t-01","attempt":1,"deadline_ms":90000}"#
    );
    let r = Record::BoardPut {
        key: key("k"),
        version: 1,
        value: Content::Text("v".into()),
    };
    assert_eq!(
        String::from_utf8(r.encode()).unwrap(),
        r#"{"kind":"board_put","key":"k","version":1,"value":{"text":"v"}}"#
    );
}

#[test]
fn a_reader_skips_fields_it_does_not_know_and_refuses_what_it_cannot_read() {
    let newer = br#"{"kind":"run_ended","outcome":"ok","cost_micros":5}"#;
    assert_eq!(
        Record::decode(newer).unwrap(),
        Record::RunEnded {
            outcome: "ok".into()
        }
    );

    assert!(Record::decode(br#"{"kind":"teleport"}"#).is_err());
    assert!(Record::decode(br#"{"kind":"task_claimed","task":"t-01","attempt":1}"#).is_err());
    // A name outside the key rule never gets in, so the viewer can read every key.
    assert!(Record::decode(br#"{"kind":"agent_gone","agent":"w 1","presence":1}"#).is_err());
    assert!(Record::decode(b"not json").is_err());
}

#[test]
fn tasks_round_trip() {
    let full = Task {
        id: task("t-07"),
        parent: task("plan-1"),
        title: "src/limiter/clock.rs".into(),
        body: Some(Content::Artifact(art("diff"))),
        lease: Some(key("shared-limiter-clock")),
        deadline_ms: Some(1_800_000_600_000),
    };
    assert_eq!(Task::decode(&full.encode()).unwrap(), full);

    let bare = Task {
        id: task("t-08"),
        parent: task("plan-1"),
        title: "x".into(),
        body: None,
        lease: None,
        deadline_ms: None,
    };
    assert_eq!(
        String::from_utf8(bare.encode()).unwrap(),
        r#"{"id":"t-08","parent":"plan-1","title":"x"}"#
    );
    assert_eq!(Task::decode(&bare.encode()).unwrap(), bare);
}

#[test]
fn stored_values_round_trip() {
    let p = Presence {
        role: "reviewer".into(),
        model: "small".into(),
        task: Some(task("t-01")),
        last_tool: Some("board_get".into()),
    };
    assert_eq!(Presence::decode(&p.encode()).unwrap(), p);
    let idle = Presence {
        role: "reviewer".into(),
        model: "small".into(),
        task: None,
        last_tool: None,
    };
    assert_eq!(Presence::decode(&idle.encode()).unwrap(), idle);

    let r = TaskResult {
        attempt: 2,
        agent: agent("w4"),
        artifact: art("result"),
    };
    assert_eq!(TaskResult::decode(&r.encode()).unwrap(), r);

    assert_eq!(lease_holder(&lease_value(&agent("w3"))), Some(agent("w3")));
    assert_eq!(lease_holder(b"not a name!"), None);
    assert_eq!(lease_holder(&[0xff, 0xfe]), None);
}
