use super::*;

#[test]
fn a_code_is_traded_once_and_only_in_time() {
    let codes = Codes::default();
    let now = Instant::now();
    let code = codes.issue("w1-a", "kblw_t".into(), now).unwrap();
    assert_eq!(codes.redeem("kblc_other", now), None);
    assert_eq!(
        codes.redeem(&code, now),
        Some(("w1-a".into(), "kblw_t".into()))
    );
    assert_eq!(
        codes.redeem(&code, now),
        None,
        "a second trade gets nothing"
    );
    let late = codes.issue("w1-b", "kblw_u".into(), now).unwrap();
    assert_eq!(codes.redeem(&late, now + CODE_TTL), None);
}

#[test]
fn waiting_codes_are_capped() {
    let codes = Codes::default();
    let now = Instant::now();
    for n in 0..MAX_CODES {
        assert!(codes.issue(&format!("w1-{n}"), "t".into(), now).is_some());
    }
    assert!(codes.issue("w1-x", "t".into(), now).is_none());
    // Expired codes make room.
    assert!(codes.issue("w1-x", "t".into(), now + CODE_TTL).is_some());
}
