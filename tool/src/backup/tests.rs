use super::*;

fn backup(records: &[&str]) -> String {
    let header = json!({
        "backup": MAGIC, "version": VERSION, "stream": WORK_EVENTS,
        "first_offset": 1, "records": records.len(), "sha256": hash_lines(records),
    });
    let mut text = format!("{header}\n");
    for r in records {
        text.push_str(r);
        text.push('\n');
    }
    text
}

#[test]
fn a_backup_reads_back_as_its_records() {
    let text = backup(&[r#"{"a":1}"#, r#"{"b":2}"#]);
    assert_eq!(
        parse_backup(&text).unwrap(),
        (1, vec![r#"{"a":1}"#, r#"{"b":2}"#])
    );
}

#[test]
fn a_cut_or_edited_backup_is_refused() {
    let text = backup(&[r#"{"a":1}"#, r#"{"b":2}"#]);
    let cut: String = text.lines().take(2).map(|l| format!("{l}\n")).collect();
    assert!(
        parse_backup(&cut)
            .unwrap_err()
            .to_string()
            .contains("cut short")
    );
    let edited = text.replace(r#"{"b":2}"#, r#"{"b":3}"#);
    assert!(
        parse_backup(&edited)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    assert!(parse_backup("{\"x\":1}\n").is_err());
    assert!(parse_backup("").is_err());
}

#[test]
fn the_digest_follows_the_state() {
    let (_, empty) = canonical(&State::default()).unwrap();
    let mut other = State::default();
    other.at = Some(4);
    assert_eq!(empty, canonical(&State::default()).unwrap().1);
    assert_ne!(empty, canonical(&other).unwrap().1);
}
