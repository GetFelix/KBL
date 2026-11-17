use super::*;

#[test]
fn a_github_timestamp_becomes_epoch_seconds() {
    assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(epoch_seconds("2026-11-10T10:00:00Z"), Some(1_794_304_800));
    assert_eq!(epoch_seconds("2024-02-29T23:59:59Z"), Some(1_709_251_199));
    assert_eq!(epoch_seconds("not a time"), None);
    assert_eq!(epoch_seconds("2026-11-10T10:00:00+01:00"), None);
}
