use super::*;
use crate::names::ItemId;

// Built at run time so no secret scanner mistakes this file for a leak.
fn fake(prefix: &str, tail: &str, n: usize) -> String {
    format!("{prefix}{}", tail.repeat(n))
}

#[test]
fn known_shapes_are_cut() {
    for secret in [
        fake("ghp_", "A1", 18),
        fake("github_pat_", "a_", 11),
        fake("glpat-", "x", 20),
        fake("sk-ant-api03-", "Ab", 20),
        fake("sk-", "z", 20),
        fake("xoxb-", "1-", 6),
        fake("AKIA", "Q7", 8),
        fake("AIza", "k", 35),
    ] {
        let text = format!("env: TOKEN={secret} done");
        assert_eq!(redact(&text), "env: TOKEN=[redacted] done", "{secret}");
    }
}

#[test]
fn a_secret_on_a_removed_diff_line_is_cut() {
    let text = format!("-{}\n+ok", fake("ghp_", "d", 36));
    assert_eq!(redact(&text), "-[redacted]\n+ok");
    let text = "------BEGIN RSA PRIVATE KEY-----\n-MIIE\n------END RSA PRIVATE KEY-----\n+x";
    assert_eq!(redact(text), "-[redacted]\n+x");
}

#[test]
fn a_long_line_of_key_headers_is_not_a_key() {
    let text = "-----BEGIN X".repeat(50_000);
    assert!(matches!(redact(&text), Cow::Borrowed(_)));
}

#[test]
fn bearer_keeps_the_scheme() {
    let text = format!("Authorization: Bearer {}", fake("", "aZ9.", 6));
    assert_eq!(redact(&text), "Authorization: Bearer [redacted]");
}

#[test]
fn private_key_block_is_cut_whole() {
    let text = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbn\nNzaC1\n-----END OPENSSH PRIVATE KEY-----\nafter";
    assert_eq!(redact(text), "before\n[redacted]\nafter");
    // A block cut off by truncated output is cut to the end.
    let text = "x -----BEGIN RSA PRIVATE KEY-----\nMIIE";
    assert_eq!(redact(text), "x [redacted]");
    // A public key is not a secret.
    let text = "-----BEGIN PUBLIC KEY-----\nMIIB";
    assert!(matches!(redact(text), Cow::Borrowed(_)));
}

#[test]
fn ordinary_text_is_left_alone() {
    for text in [
        "cargo test -p kbl-core redact::",
        "disk-abcdefghijklmnopqrstuvwxyz is a word, not a key",
        "ghp_short",
        "AKIAlowercase0000000000",
        "Bearer short",
        "caf\u{e9} sk-",
    ] {
        assert!(matches!(redact(text), Cow::Borrowed(_)), "{text}");
    }
}

#[test]
fn non_ascii_around_a_secret() {
    let text = format!("\u{e9}t\u{e9} {} \u{1f431}", fake("ghs_", "b", 36));
    assert_eq!(redact(&text), "\u{e9}t\u{e9} [redacted] \u{1f431}");
}

#[test]
fn records_have_their_free_text_cut() {
    let secret = fake("ghp_", "c", 36);
    let mut step = WorkRecord::Step {
        item: ItemId::new("a1").unwrap(),
        attempt: 1,
        command: format!("curl -H 'Authorization: token {secret}'"),
        exit_code: 0,
        output: Content::Text(format!("echo {secret}")),
    };
    redact_record(&mut step);
    let WorkRecord::Step {
        command, output, ..
    } = step
    else {
        unreachable!()
    };
    assert_eq!(command, "curl -H 'Authorization: token [redacted]'");
    assert_eq!(output, Content::Text("echo [redacted]".into()));
}
