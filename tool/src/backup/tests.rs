use std::sync::Mutex;

use super::*;

/// A one-shard log. `raft` makes a refused conditional publish take an offset,
/// as a Raft shard does; `lead` blanks are already there, like a new leader's.
struct Fake {
    raft: bool,
    /// Stop publishing after this many writes, as a dropped connection would.
    cut_after: Mutex<Option<usize>>,
    slots: Mutex<Vec<Option<Vec<u8>>>>,
}

impl Fake {
    fn new(raft: bool, lead: usize) -> Self {
        Fake {
            raft,
            cut_after: Mutex::new(None),
            slots: Mutex::new(vec![None; lead]),
        }
    }

    fn cut(&self, after: usize) {
        *self.cut_after.lock().unwrap() = Some(after);
    }

    fn uncut(&self) {
        *self.cut_after.lock().unwrap() = None;
    }

    fn append(&self, payload: &[u8]) {
        self.slots.lock().unwrap().push(Some(payload.to_vec()));
    }

    fn spend(&self) -> Result<()> {
        let mut cut = self.cut_after.lock().unwrap();
        match cut.as_mut() {
            Some(0) => bail!("the connection dropped"),
            Some(n) => *n -= 1,
            None => {}
        }
        Ok(())
    }

    fn records(&self) -> Vec<(u64, Vec<u8>)> {
        let slots = self.slots.lock().unwrap();
        (0u64..)
            .zip(slots.iter())
            .filter_map(|(o, s)| s.clone().map(|p| (o, p)))
            .collect()
    }

    fn tip(&self) -> u64 {
        self.slots.lock().unwrap().len() as u64
    }
}

impl LogStore for Fake {
    async fn read_to_tip(&self, _: &str) -> Result<(Vec<(u64, Vec<u8>)>, u64)> {
        Ok((self.records(), self.tip()))
    }

    async fn publish(&self, _: &str, payload: Vec<u8>) -> Result<u64> {
        self.spend()?;
        self.slots.lock().unwrap().push(Some(payload));
        Ok(self.tip() - 1)
    }

    async fn publish_if(&self, _: &str, _: Vec<u8>, expected: u64) -> Result<ConditionalWrite> {
        self.spend()?;
        let tail = self.tip();
        assert_ne!(expected, tail, "the test's refusal must be a refusal");
        if self.raft {
            self.slots.lock().unwrap().push(None);
            return Ok(ConditionalWrite::Refused { tail: tail + 1 });
        }
        Ok(ConditionalWrite::Refused { tail })
    }
}

fn backup(records: &[&str]) -> String {
    let lines: Vec<&str> = records.to_vec();
    let header = json!({
        "backup": MAGIC, "version": VERSION, "stream": WORK_EVENTS,
        "first_offset": 1, "next_offset": 1 + lines.len(), "records": lines.iter().filter(|l| !l.is_empty()).count(),
        "sha256": hash_lines(&lines),
    });
    let mut text = format!("{header}\n");
    for r in records {
        text.push_str(r);
        text.push('\n');
    }
    text
}

fn parsed(text: &str) -> Backup<'_> {
    parse_backup(Log::Work, text).unwrap()
}

#[test]
fn a_backup_reads_back_as_its_offsets() {
    let text = backup(&[r#"{"a":1}"#, "", r#"{"b":2}"#, ""]);
    let b = parsed(&text);
    assert_eq!((b.first, b.next), (1, 5));
    assert_eq!(
        b.records().collect::<Vec<_>>(),
        vec![(1, r#"{"a":1}"#), (3, r#"{"b":2}"#)]
    );
    assert_eq!(b.line(2), None);
}

#[test]
fn a_cut_or_edited_backup_is_refused() {
    let text = backup(&[r#"{"a":1}"#, r#"{"b":2}"#]);
    let cut: String = text.lines().take(2).map(|l| format!("{l}\n")).collect();
    let err = |t: &str| parse_backup(Log::Work, t).err().unwrap().to_string();
    assert!(err(&cut).contains("cut short"));
    assert!(err(&text.replace(r#"{"b":2}"#, r#"{"b":3}"#)).contains("checksum"));
    // A blank moved to another offset changes the checksum too.
    let moved = backup(&["", r#"{"a":1}"#]).replace(
        &hash_lines(&["", r#"{"a":1}"#]),
        &hash_lines(&[r#"{"a":1}"#, ""]),
    );
    assert!(err(&moved).contains("checksum"));
    assert!(err("{\"x\":1}\n").contains("not a kbl backup"));
    assert!(err("").contains("empty"));
    // Another log's backup is not this log's.
    assert!(parse_backup(Log::Org, &text).is_err());
}

#[test]
fn the_digest_follows_the_state() {
    let (_, empty) = canonical(&State::default()).unwrap();
    let mut other = State::default();
    other.at = Some(4);
    assert_eq!(empty, canonical(&State::default()).unwrap().1);
    assert_ne!(empty, canonical(&other).unwrap().1);
}

#[test]
fn an_export_keeps_every_blank_offset() {
    // Records at 2, 4 and 5; blanks at 3 and 6 (a trailing one).
    let recs = vec![
        (2, br#"{"a":1}"#.to_vec()),
        (4, br#"{"b":2}"#.to_vec()),
        (5, br#"{"c":3}"#.to_vec()),
    ];
    let (text, _) = render(Log::Work, &recs, 7).unwrap();
    let b = parsed_owned(&text);
    assert_eq!((b.0, b.1), (2, 7));
    assert_eq!(b.2, vec![r#"{"a":1}"#, "", r#"{"b":2}"#, r#"{"c":3}"#, ""]);
    // A record the file could not hold on a line is refused.
    assert!(render(Log::Work, &[(0, b"a\nb".to_vec())], 1).is_err());
}

fn parsed_owned(text: &str) -> (u64, u64, Vec<String>) {
    let b = parsed(text);
    (
        b.first,
        b.next,
        b.lines.iter().map(|l| l.to_string()).collect(),
    )
}

fn text_with_blanks() -> String {
    backup(&[r#"{"a":1}"#, "", r#"{"b":2}"#, "", ""])
}

#[tokio::test]
async fn blanks_come_back_at_their_offsets_on_a_raft_log() {
    let text = text_with_blanks();
    // The new log has its leader's blank at 0, like the source did.
    let fake = Fake::new(true, 1);
    restore(&fake, WORK_EVENTS, &parsed(&text)).await.unwrap();
    let want = vec![(1, br#"{"a":1}"#.to_vec()), (3, br#"{"b":2}"#.to_vec())];
    assert_eq!((fake.records(), fake.tip()), (want, 6));
}

#[tokio::test]
async fn a_log_without_blanks_restores_on_either_broker() {
    let text = backup(&[r#"{"a":1}"#, r#"{"b":2}"#]);
    for raft in [true, false] {
        let fake = Fake::new(raft, 1);
        restore(&fake, WORK_EVENTS, &parsed(&text)).await.unwrap();
        assert_eq!(fake.records().len(), 2);
    }
}

#[tokio::test]
async fn blanks_are_refused_where_a_refusal_takes_no_offset() {
    let text = text_with_blanks();
    let fake = Fake::new(false, 1);
    let err = restore(&fake, WORK_EVENTS, &parsed(&text))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("blank at offset 2") && err.contains("Raft"),
        "{err}"
    );
}

#[tokio::test]
async fn a_cut_restore_carries_on_where_it_stopped() {
    let text = text_with_blanks();
    let fake = Fake::new(true, 1);
    // The connection drops after the first record and the blank.
    fake.cut(2);
    assert!(restore(&fake, WORK_EVENTS, &parsed(&text)).await.is_err());
    assert_eq!(fake.tip(), 3);
    fake.uncut();
    restore(&fake, WORK_EVENTS, &parsed(&text)).await.unwrap();
    assert_eq!((fake.records().len(), fake.tip()), (2, 6));
    // Done is done: a third run changes nothing.
    let done = restore(&fake, WORK_EVENTS, &parsed(&text)).await.unwrap();
    assert!(done.contains("already holds"), "{done}");
    assert_eq!(fake.tip(), 6);
}

#[tokio::test]
async fn a_log_that_holds_something_else_is_refused() {
    let text = backup(&[r#"{"a":1}"#, r#"{"b":2}"#]);
    // A different record where the backup's first goes.
    let other = Fake::new(true, 1);
    other.append(br#"{"z":9}"#);
    let err = restore(&other, WORK_EVENTS, &parsed(&text))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not"), "{err}");
    // A log that runs past the backup.
    let longer = Fake::new(true, 1);
    for r in [r#"{"a":1}"#, r#"{"b":2}"#, r#"{"c":3}"#] {
        longer.append(r.as_bytes());
    }
    let err = restore(&longer, WORK_EVENTS, &parsed(&text))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not"), "{err}");
    // A log that ends past the backup holding only blanks.
    let blanks = Fake::new(true, 5);
    let err = restore(&blanks, WORK_EVENTS, &parsed(&text))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("past the backup"), "{err}");
    // A blank where the backup has a record.
    let gap = Fake::new(true, 1);
    gap.slots.lock().unwrap().push(None);
    gap.append(br#"{"b":2}"#);
    assert!(restore(&gap, WORK_EVENTS, &parsed(&text)).await.is_err());
}

#[tokio::test]
async fn a_log_with_no_leader_blank_is_padded_to_the_backups_first_offset() {
    // The backup starts at offset 1; this log is empty.
    let text = backup(&[r#"{"a":1}"#]);
    let raft = Fake::new(true, 0);
    restore(&raft, WORK_EVENTS, &parsed(&text)).await.unwrap();
    assert_eq!(raft.records(), vec![(1, br#"{"a":1}"#.to_vec())]);
    // Where no offset can be burned, the record would land at 0: refused.
    let hybrid = Fake::new(false, 0);
    let err = restore(&hybrid, WORK_EVENTS, &parsed(&text))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("blank at offset 0"), "{err}");
    assert_eq!(hybrid.tip(), 0);
}

#[cfg(unix)]
#[test]
fn an_export_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("kbl-backup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.backup");
    let path = path.to_str().unwrap();
    // A file that already exists with wider rights is narrowed too.
    fs::write(path, "old").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    write_file(path, "new").unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), "new");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn a_digest_of_the_exported_prefix_ignores_later_writes() {
    let recs = vec![(0, br#"{"a":1}"#.to_vec()), (1, br#"{"b":2}"#.to_vec())];
    let (_, whole) = digest_json(Log::Org, &recs).unwrap();
    let mut later = recs.clone();
    later.push((2, br#"{"c":3}"#.to_vec()));
    let prefix: Vec<_> = later.iter().filter(|(o, _)| *o < 2).cloned().collect();
    assert_eq!(digest_json(Log::Org, &prefix).unwrap().1, whole);
    assert_ne!(digest_json(Log::Org, &later).unwrap().1, whole);
}
