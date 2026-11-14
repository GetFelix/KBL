//! The format rules of `docs/upgrades.md` for a run's own records: run log
//! records and tasks of the first format are written as they always were, and
//! a reader refuses one of a newer format instead of skipping it.

use kbl_core::codec::Codec;
use kbl_core::names::{Key, TaskId};
use kbl_core::record::{Logged, RUN_LOG_FORMAT, RUN_LOG_FORMAT_V1, Record, RunLogReadError};
use kbl_core::task::{TASK_FORMAT, TASK_FORMAT_V1, Task, TaskReadError};
use kbl_core::{record, task};

/// Every run log kind of the first format. Frozen: a new kind is not added
/// here, it gets a higher format in `Record::format`.
const V1_KINDS: [&str; 24] = [
    "run_started",
    "run_ended",
    "agent_up",
    "agent_gone",
    "task_posted",
    "task_claimed",
    "task_extended",
    "task_released",
    "task_done",
    "task_duplicate",
    "task_dead",
    "board_put",
    "board_conflict",
    "lease_taken",
    "lease_renewed",
    "lease_released",
    "lease_lost",
    "message",
    "model_started",
    "model_call",
    "budget_exhausted",
    "refund_unconfirmed",
    "refused",
    "task_claim_lost",
];

/// `wire.jsonl` is every kind as the encoder wrote it before formats. Logged,
/// each is the same bytes behind the time and the writer, with no `v`.
#[test]
fn a_first_format_run_log_record_is_written_with_the_bytes_it_always_had() {
    let by = Key::new("w3").unwrap();
    let mut seen = Vec::new();
    for (i, line) in include_str!("wire.jsonl").lines().enumerate() {
        let rec = Record::decode(line.as_bytes()).unwrap();
        assert_ne!(rec.kind(), "unknown", "line {i}");
        assert_eq!(rec.format(), RUN_LOG_FORMAT_V1, "line {i}");
        seen.push(rec.kind());
        let logged = Logged::new(1_800_000_000_000, by.clone(), rec);
        let want = format!(r#"{{"at_ms":1800000000000,"by":"w3",{}"#, &line[1..]);
        assert_eq!(
            String::from_utf8(logged.encode()).unwrap(),
            want,
            "line {i}"
        );
        assert_eq!(record::read(want.as_bytes()).unwrap(), logged, "line {i}");
        // Naming the first format changes nothing.
        let named = want.replacen('{', r#"{"v":1,"#, 1);
        assert_eq!(record::read(named.as_bytes()).unwrap(), logged, "line {i}");
    }
    seen.sort_unstable();
    seen.dedup();
    let mut frozen = V1_KINDS.to_vec();
    frozen.sort_unstable();
    assert_eq!(seen, frozen);
}

const NEWER: &str = r#"{"v":999,"at_ms":5,"by":"w1","kind":"run_ended","outcome":"ok"}"#;

#[test]
fn a_run_log_record_of_a_newer_format_is_refused_not_skipped() {
    assert!(matches!(
        record::read(NEWER.as_bytes()),
        Err(RunLogReadError::Newer { v: 999 })
    ));
    // The plain decoder cannot be used to get around it.
    assert!(Logged::decode(NEWER.as_bytes()).is_err());
    // A kind this build has never heard of is refused the same way, where a
    // record of a readable format is skipped as `unknown`.
    let kind = r#"{"v":999,"at_ms":5,"by":"w1","kind":"teleported"}"#;
    assert!(matches!(
        record::read(kind.as_bytes()),
        Err(RunLogReadError::Newer { v: 999 })
    ));
    let old = r#"{"at_ms":5,"by":"w1","kind":"teleported"}"#;
    assert_eq!(
        record::read(old.as_bytes()).unwrap().record,
        Record::Unknown
    );
    let next = NEWER.replacen("999", &(RUN_LOG_FORMAT + 1).to_string(), 1);
    assert!(matches!(
        record::read(next.as_bytes()),
        Err(RunLogReadError::Newer { .. })
    ));
}

#[test]
fn a_run_log_version_that_is_not_a_format_is_malformed() {
    for v in ["0", "-1", "\"2\"", "1.5", "null"] {
        let line = NEWER.replacen("999", v, 1);
        assert!(
            matches!(
                record::read(line.as_bytes()),
                Err(RunLogReadError::Malformed(_))
            ),
            "v = {v}"
        );
    }
    assert!(matches!(
        record::read(b"not json"),
        Err(RunLogReadError::Malformed(_))
    ));
}

/// A task as `kbl-mcp` posted it before formats, with every field set.
const OLD_TASK: &str = r#"{"id":"t-07","parent":"plan-1","title":"src/a.rs","body":{"text":"look for bugs"},"lease":"shared-a","deadline_ms":1800000600000}"#;

#[test]
fn a_first_format_task_is_written_with_the_bytes_it_always_had() {
    let old = task::read(OLD_TASK.as_bytes()).unwrap();
    assert_eq!(old.v, TASK_FORMAT_V1);
    assert_eq!(old.format(), TASK_FORMAT_V1);
    assert_eq!(String::from_utf8(old.encode()).unwrap(), OLD_TASK);
    let named = OLD_TASK.replacen('{', r#"{"v":1,"#, 1);
    assert_eq!(task::read(named.as_bytes()).unwrap(), old);

    let bare = Task::new(
        TaskId::new("t-08").unwrap(),
        TaskId::new("plan-1").unwrap(),
        "x".into(),
    )
    .stamped();
    assert_eq!(
        String::from_utf8(bare.encode()).unwrap(),
        r#"{"id":"t-08","parent":"plan-1","title":"x"}"#
    );
}

#[test]
fn a_task_of_a_newer_format_is_refused_not_read_as_an_old_one() {
    let newer = OLD_TASK.replacen('{', r#"{"v":999,"#, 1);
    assert!(matches!(
        task::read(newer.as_bytes()),
        Err(TaskReadError::Newer { v: 999 })
    ));
    assert!(Task::decode(newer.as_bytes()).is_err());
    let next = OLD_TASK.replacen('{', &format!(r#"{{"v":{},"#, TASK_FORMAT + 1), 1);
    assert!(matches!(
        task::read(next.as_bytes()),
        Err(TaskReadError::Newer { .. })
    ));
    for v in ["0", "-1", "\"2\"", "1.5", "null"] {
        let line = newer.replacen("999", v, 1);
        assert!(
            matches!(
                task::read(line.as_bytes()),
                Err(TaskReadError::Malformed(_))
            ),
            "v = {v}"
        );
    }
    assert!(matches!(
        task::read(b"not json"),
        Err(TaskReadError::Malformed(_))
    ));
}
