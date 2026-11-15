use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::Value;

use super::*;
use crate::names::ItemId;

// Built at run time so no secret scanner mistakes this file for a leak.
fn fake(prefix: &str, tail: &str, n: usize) -> String {
    format!("{prefix}{}", tail.repeat(n))
}

/// A full line of a PEM body.
const BODY: &str = "MIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9Zx";

fn untouched(text: &str) -> bool {
    matches!(redact(text), Cow::Borrowed(_))
}

#[test]
fn known_shapes_are_cut() {
    for secret in [
        fake("ghp_", "A1", 18),
        fake("github_pat_", "a_", 11),
        fake("glpat-", "x", 20),
        fake("sk-ant-api03-", "Ab3x", 20),
        fake("sk-proj-", "Zq9xT3Bl", 6),
        fake("sk-", "Kf7", 16),
        fake("sk_live_", "4eC3", 6),
        fake("rk_live_", "9aB", 8),
        fake("npm_", "a1B", 12),
        fake("xapp-1-A0123BCDEFG-1234567890123-", "a1b2", 16),
        fake("xoxb-123456789012-1234567890123-", "AbC4", 6),
        fake("xoxp-12345678-23456789-34567890-", "f0e1", 8),
        fake("AKIA", "Q7", 8),
        fake("AIza", "k", 35),
        format!(
            "{}.{}.{}",
            fake("eyJ", "hbGc", 4),
            fake("eyJ", "zdW", 3),
            fake("", "Sf3_", 6)
        ),
    ] {
        let text = format!("env: X={secret} done");
        assert_eq!(redact(&text), "env: X=[redacted] done", "{secret}");
    }
}

#[test]
fn a_secret_on_a_removed_diff_line_is_cut() {
    let text = format!("-{}\n+ok", fake("ghp_", "d", 36));
    assert_eq!(redact(&text), "-[redacted]\n+ok");
    let text =
        format!("------BEGIN RSA PRIVATE KEY-----\n-{BODY}\n------END RSA PRIVATE KEY-----\n+x");
    assert_eq!(redact(&text), "-[redacted]\n+x");
}

#[test]
fn json_escapes_and_delimiters_start_a_secret() {
    let gh = fake("ghp_", "e", 36);
    for before in [
        "\\n", "\\t", "\\r", "\\\"", "\\\\", "\\u0022", "TOKEN_", "TOKEN=", "token:",
    ] {
        let text = format!("{{\"out\":\"x{before}{gh}\\n\"}}");
        assert_eq!(
            redact(&text),
            format!("{{\"out\":\"x{before}[redacted]\\n\"}}"),
            "{before}"
        );
    }
    // A key block inside a JSON string, its lines joined by `\n` escapes.
    let text = format!(
        "{{\"out\":\"-----BEGIN RSA PRIVATE KEY-----\\n{BODY}\\nAB==\\n-----END RSA PRIVATE KEY-----\\nok\"}}"
    );
    assert_eq!(redact(&text), "{\"out\":\"[redacted]\\nok\"}");
    let text =
        format!("\"a\\n-----BEGIN EC PRIVATE KEY-----\\n{BODY}\\n-----END EC PRIVATE KEY-----\"");
    assert_eq!(redact(&text), "\"a\\n[redacted]\"");
}

#[test]
fn back_to_back_secrets_are_cut_whole() {
    let (a, b) = (fake("ghp_", "A", 36), fake("ghp_", "B", 36));
    assert_eq!(redact(&format!("{a}{b} x")), "[redacted] x");
    assert_eq!(redact(&format!("{a},{b}")), "[redacted],[redacted]");
    let aws = fake("AKIA", "Z", 16);
    assert_eq!(redact(&format!("{aws}_{a}")), "[redacted]");
    // A short look-alike just before a real one does not hide it.
    assert_eq!(redact(&format!("ghp_abc_{b}")), "ghp_abc_[redacted]");
    assert_eq!(
        redact(&format!("sk-sk-{}", fake("", "Kf7", 16))),
        "[redacted]"
    );
}

#[test]
fn every_pattern_starts_with_a_first_byte() {
    let starts = SHAPES.iter().map(|s| s.prefix).chain([
        KEY_BEGIN, "bearer ", "Bearer ", "basic ", "Basic ", "token=", "TOKEN=", "eyJ",
    ]);
    for p in starts {
        assert!(FIRST.contains(&p.as_bytes()[0]), "{p}");
    }
}

#[test]
fn a_long_line_of_key_headers_is_not_a_key() {
    let text = "-----BEGIN X".repeat(50_000);
    assert!(untouched(&text));
}

#[test]
fn bearer_basic_and_query_tokens_keep_their_name() {
    let t = fake("", "aZ9.", 6);
    for scheme in ["Bearer", "bearer", "BEARER"] {
        let text = format!("Authorization: {scheme} {t}");
        assert_eq!(redact(&text), format!("Authorization: {scheme} [redacted]"));
    }
    // `user:password`, base64.
    let text = "authorization: Basic dXNlcjpwYXNzd29yZA==";
    assert_eq!(redact(text), "authorization: Basic [redacted]");
    let text = format!("GET /x?a=1&access_token={}&b=2", fake("", "q8", 10));
    assert_eq!(redact(&text), "GET /x?a=1&access_token=[redacted]&b=2");
    let text = format!("curl https://h/?token={}", fake("", "Z", 12));
    assert_eq!(redact(&text), "curl https://h/?token=[redacted]");
}

#[test]
fn url_userinfo_is_cut() {
    let text = "git clone https://x-access-token:s3cretValue@github.com/o/r";
    assert_eq!(redact(text), "git clone https://[redacted]@github.com/o/r");
    // Shorter than the marker: as many stars, so the text never grows.
    assert_eq!(redact("postgres://u:pw@db/x"), "postgres://****@db/x");
}

#[test]
fn private_key_block_is_cut_whole() {
    let text = format!(
        "before\n-----BEGIN OPENSSH PRIVATE KEY-----\n{BODY}\nNzaC1\n-----END OPENSSH PRIVATE KEY-----\nafter"
    );
    assert_eq!(redact(&text), "before\n[redacted]\nafter");
    // A block cut off by truncated output loses its body lines.
    let text = format!("x -----BEGIN RSA PRIVATE KEY-----\n{BODY}");
    assert_eq!(redact(&text), "x [redacted]");
    let line = "A".repeat(64);
    let text = format!("-----BEGIN RSA PRIVATE KEY-----\n{line}\n{line}\nAB==\n$ next");
    assert_eq!(redact(&text), "[redacted]\n$ next");
    // A public key is not a secret.
    assert!(untouched("-----BEGIN PUBLIC KEY-----\nMIIB"));
}

#[test]
fn a_key_header_without_its_end_cuts_only_the_header() {
    // A grep line that names the header.
    let text = "src/pem.rs:12:const RSA: &str = \"-----BEGIN RSA PRIVATE KEY-----\"; // marker\nsrc/pem.rs:13:fn parse() {}";
    assert_eq!(
        redact(text),
        "src/pem.rs:12:const RSA: &str = \"[redacted]\"; // marker\nsrc/pem.rs:13:fn parse() {}"
    );
    // A test file's diff hunk.
    let text = "@@ -1,2 +1,3 @@\n+const H: &str = \"-----BEGIN EC PRIVATE KEY-----\";\n+fn is_key(s: &str) -> bool { s.starts_with(H) }\n fn other() {}";
    assert_eq!(
        redact(text),
        "@@ -1,2 +1,3 @@\n+const H: &str = \"[redacted]\";\n+fn is_key(s: &str) -> bool { s.starts_with(H) }\n fn other() {}"
    );
    // An `END` of another type does not close the block.
    let text = "-----BEGIN RSA PRIVATE KEY-----\nfoo bar\n-----END EC PRIVATE KEY-----";
    assert_eq!(
        redact(text),
        "[redacted]\nfoo bar\n-----END EC PRIVATE KEY-----"
    );
}

#[test]
fn look_alikes_are_left_alone() {
    for text in [
        "cargo test -p kbl-core redact::",
        "disk-abcdefghijklmnopqrstuvwxyz is a word, not a key",
        "ghp_short",
        "AKIAlowercase0000000000",
        "Bearer short",
        "the bearer of bad news brought it",
        "caf\u{e9} sk-",
        // scikit-learn's HTML repr.
        "<div class=\"sk-estimator-doc-link-fitted sk-toggleable__content-wrapper\">",
        ".sk-toggleable__content-wrapper-and-some-more-words { display: none }",
        "sk-very-long-identifier-made-of-many-small-words-only",
        "git checkout feature/AKIAIOSFODNN7EXAMPLE1",
        "a Basic premises section and basic authentication",
        "max_token=4096 and ?token=$TOKEN and token=abc",
        "eyJhbGciOiJIUzI1NiJ9 alone is a header",
        "npm_config_cache=/tmp/npm",
        "xapp-config.json",
        "sk_live_mode",
        "https://github.com/o/r and ssh://git@github.com/o/r",
        "scheme://host:8080/path@v2",
    ] {
        assert!(untouched(text), "{text}");
    }
}

#[test]
fn non_ascii_around_a_secret() {
    let text = format!("\u{e9}t\u{e9} {} \u{1f431}", fake("ghs_", "b", 36));
    assert_eq!(redact(&text), "\u{e9}t\u{e9} [redacted] \u{1f431}");
}

#[test]
fn adversarial_input_stays_linear() {
    // Each is 10 MB of what would make a scanner backtrack or rescan.
    let mb = 10 * 1024 * 1024;
    let inputs = [
        "-----BEGIN RSA PRIVATE KEY-----".repeat(mb / 31),
        "-----BEGIN RSA PRIVATE KEY-----\n-----END EC PRIVATE KEY-----\n".repeat(mb / 60),
        format!("-----BEGIN RSA PRIVATE KEY-----{}", "\n".repeat(mb)),
        "a://".repeat(mb / 4),
        format!("a://{}", "u:".repeat(mb / 2)),
        "eyJxxxxxxxxxx.".repeat(mb / 14),
        "Bearer ".repeat(mb / 7),
        "sk-".repeat(mb / 3),
        "xoxb-".repeat(mb / 5),
        "xapp-1-".repeat(mb / 7),
        "token=".repeat(mb / 6),
        format!("?token={}", "token=a1".repeat(mb / 8)),
        "Authorization: Basic ".repeat(mb / 21),
        "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n".repeat(mb / 40) + "-----END RSA PRIVATE KEY-----",
        format!(
            "{}-----END RSA PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-\n".repeat(400)
        )
        .repeat(mb / 16_000),
        format!("-----BEGIN RSA PRIVATE KEY-----{}", " ".repeat(mb)),
        "-----BEGIN RSA PRIVATE KEY-----\n    > ".repeat(mb / 38),
        format!("-----BEGIN RSA PRIVATE KEY----- {}", "A ".repeat(mb / 2)),
        "xoxe.xoxb-".repeat(mb / 10),
        format!(
            "-----BEGIN RSA PRIVATE KEY-----\n{}{BODY}\n",
            " ".repeat(120)
        )
        .repeat(mb / 220),
        format!(
            "-----BEGIN RSA PRIVATE KEY-----{}",
            "\n-----END ".repeat(mb / 10)
        ),
        format!("xoxb-1-{}", "A-".repeat(mb / 2)),
        format!("sk-{}", "a-".repeat(mb / 2)),
        "\\n".repeat(mb / 2),
        "A".repeat(mb),
    ];
    for text in inputs {
        let start = Instant::now();
        let _ = redact(&text);
        let took = start.elapsed();
        // Generous for a debug build on a loaded runner; quadratic would take hours.
        assert!(took.as_secs() < 20, "{:?} took {took:?}", &text[..20]);
    }
}

fn secret() -> String {
    fake("ghp_", "c", 36)
}

/// The path of every string in `v`.
fn leaves(v: &Value, path: String, out: &mut Vec<String>) {
    match v {
        Value::String(_) => out.push(path),
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                leaves(x, format!("{path}/{i}"), out);
            }
        }
        Value::Object(o) => {
            for (k, x) in o {
                leaves(x, format!("{path}/{k}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_free_text_field_of_every_kind_is_cut() {
    // Each wire line, with one string at a time set to a secret. A field
    // that still holds it after redaction must be one `redact_record` lists
    // as structured.
    let structured: BTreeSet<&str> = [
        "imported/source",
        "imported/upstream_id",
        "proposed/key",
        "proposed/review/tier",
        "proposed/review/pr",
        "progress/branch",
        "progress/pr",
        "prerequisite/key",
        "prerequisite/new/target/source",
        "prerequisite/new/target/repo",
        "prerequisite/new/evidence/0/stream",
        "context/entry",
        "answer/evidence/0/stream",
        "answer/then/target/source",
        "answer/then/target/repo",
        "decision/evidence/0/stream",
        "pr_status/status/pr",
        "source_stalled/source",
        "source_stalled/repo",
        "rules_set/target/source",
        "rules_set/target/repo",
        "refused/op",
    ]
    .into();
    let mut survived = BTreeSet::new();
    let mut cut = BTreeSet::new();
    // Fields the wire lines leave out.
    let extra = [
        r#"{"kind":"triaged","item":"r1","tier":1,"why":"w","split":["a","b"]}"#,
        r#"{"kind":"released","item":"r1","attempt":1,"reason":"r"}"#,
        r#"{"kind":"edited","item":"r1","title":"t"}"#,
    ];
    let wire = include_str!("../../tests/work_wire.jsonl").lines();
    for line in wire.chain(extra) {
        let v: Value = serde_json::from_str(line).unwrap();
        let kind = v["kind"].as_str().unwrap().to_owned();
        let mut paths = Vec::new();
        leaves(&v, String::new(), &mut paths);
        for path in paths {
            if path == "/kind" {
                continue;
            }
            let mut v = v.clone();
            *v.pointer_mut(&path).unwrap() = Value::String(format!("x {} y", secret()));
            // A field with a fixed shape (an id, an enum) refuses the secret.
            let Ok(mut record) = serde_json::from_value::<WorkRecord>(v) else {
                continue;
            };
            redact_record(&mut record);
            let field = format!("{kind}{path}")
                .split('/')
                .map(|s| if s.parse::<usize>().is_ok() { "0" } else { s })
                .collect::<Vec<_>>()
                .join("/");
            if serde_json::to_string(&record).unwrap().contains(&secret()) {
                survived.insert(field);
            } else {
                cut.insert(field);
            }
        }
    }
    let leaked: Vec<_> = survived
        .iter()
        .filter(|f| !structured.contains(f.as_str()))
        .collect();
    assert!(leaked.is_empty(), "free text kept a secret: {leaked:?}");
    // The fields the review named, each exercised.
    for f in [
        "proposed/title",
        "prerequisite/new/title",
        "prerequisite/new/why/text",
        "question/options/0",
        "question/default",
        "answer/chosen",
        "answer/then/title",
        "triaged/split/0",
        "decision/commit",
        "decision/revert_check/test",
        "finding/file",
        "edit/file",
        "human_input/text/text",
        "edited/title",
        "released/reason",
    ] {
        assert!(cut.contains(f), "{f} was not exercised or not cut");
    }
}

#[test]
fn records_have_their_free_text_cut() {
    let secret = secret();
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

#[test]
fn a_progress_pr_and_branch_keep_their_words() {
    let id = || ItemId::new("a1").unwrap();
    let progress = |pr: &str, branch: &str| WorkRecord::Progress {
        item: id(),
        attempt: 1,
        note: Content::Text("n".into()),
        branch: Some(branch.into()),
        pr: Some(pr.into()),
    };
    for (pr, branch) in [
        ("acme/xoxb-slackbot-tools#7", "xoxb-slackbot-tools/fix"),
        (
            "acme/sk-learn-extras-and-many-more#12",
            "feature/xapp-redesign-of-the-settings-page",
        ),
        (
            "acme/token=abc0123456789abcdefghijk#3",
            "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
        ),
    ] {
        let mut r = progress(pr, branch);
        redact_record(&mut r);
        assert_eq!(r, progress(pr, branch), "{pr} {branch}");
    }
    // Userinfo in a URL is still cut.
    let mut r = progress("https://x:s3cretValue@github.com/acme/r/pull/7", "b");
    redact_record(&mut r);
    assert_eq!(
        r,
        progress("https://[redacted]@github.com/acme/r/pull/7", "b")
    );
}

#[test]
fn prose_and_code_that_name_a_scheme_are_left_alone() {
    for text in [
        "## Basic Installation",
        "Basic Requirements",
        "fn f() -> basic HttpResponse",
        "Client(token=os.environ.get(\"GH\"))",
        "let t = Client { token=self.token_cache.clone() };",
        "next_token=settings.API_TOKEN",
        "token=getTokenFromVault2024abcdef()",
        "feature/xapp-redesign-of-the-settings-page",
        "git checkout xoxb-slackbot-tools-and-more",
        "sk-add-TokenValidatorForUsers-to-the-api-layer",
        "see bearer authentication/authorization-flow",
    ] {
        assert!(untouched(text), "{text}");
    }
    // The same values in a header or a query are cut.
    assert_eq!(
        redact("Authorization: Basic dXNlcjpwYXNz"),
        "Authorization: Basic [redacted]"
    );
    assert_eq!(
        redact("{\"authorization\": \"basic dXNlcjpwYXNz\"}"),
        "{\"authorization\": \"basic [redacted]\"}"
    );
    assert_eq!(redact("x Basic dXNlcjpwYXNzd29yZDE="), "x Basic [redacted]");
    assert_eq!(
        redact("GET /cb?next_token=settings.API_TOKEN"),
        "GET /cb?next_token=[redacted]"
    );
    let t = fake("", "q8Z", 8);
    assert_eq!(redact(&format!("token={t}")), "token=[redacted]");
}

#[test]
fn key_markers_in_code_are_not_a_block() {
    let text = r#"if s.starts_with("-----BEGIN PRIVATE KEY-----") && s.ends_with("-----END PRIVATE KEY-----") {"#;
    assert_eq!(
        redact(text),
        r#"if s.starts_with("[redacted]") && s.ends_with("-----END PRIVATE KEY-----") {"#
    );
    let text = "@@ -1,2 +1,2 @@\n-const BEGIN: &str = \"-----BEGIN PRIVATE KEY-----\";\n+const END: &str = \"-----END PRIVATE KEY-----\";\n fn f() {}";
    assert_eq!(
        redact(text),
        "@@ -1,2 +1,2 @@\n-const BEGIN: &str = \"[redacted]\";\n+const END: &str = \"-----END PRIVATE KEY-----\";\n fn f() {}"
    );
    // A key in a diff, every line marked, is still cut whole.
    let line = "A".repeat(70);
    let text = format!(
        "+-----BEGIN OPENSSH PRIVATE KEY-----\n+{line}\n+AB==\n+-----END OPENSSH PRIVATE KEY-----\n fn f() {{}}"
    );
    assert_eq!(redact(&text), "+[redacted]\n fn f() {}");
    let text = format!(
        "------BEGIN OPENSSH PRIVATE KEY-----\n-{line}\n-AB==\n------END OPENSSH PRIVATE KEY-----\n"
    );
    assert_eq!(redact(&text), "-[redacted]\n");
}

#[test]
fn an_indented_or_quoted_key_is_cut_whole() {
    let line = fake("MIIE", "q9Zx", 15);
    let short = "AB12cd==";
    let key = |pre: &str, sep: &str| {
        format!(
            "{pre}-----BEGIN PRIVATE KEY-----{sep}{pre}{line}{sep}{pre}{short}{sep}{pre}-----END PRIVATE KEY-----"
        )
    };
    // A YAML block scalar.
    let text = format!("tls.key: |\n{}\nnext: 1", key("    ", "\n"));
    assert_eq!(redact(&text), "tls.key: |\n    [redacted]\nnext: 1");
    // A diff of one, a quoted reply, a tab-indented one.
    for pre in ["+    ", "> ", "\t", "-  "] {
        let text = format!("{}\n fn f() {{}}", key(pre, "\n"));
        assert_eq!(
            redact(&text),
            format!("{pre}[redacted]\n fn f() {{}}"),
            "{pre:?}"
        );
    }
    // Newlines turned into spaces.
    let text = format!("KEY=\"{}\" rest", key("", " "));
    assert_eq!(redact(&text), "KEY=\"[redacted]\" rest");
    // Cut off before its `END`: the indented and space-split lines still go.
    let text = format!("  -----BEGIN PRIVATE KEY-----\n  {line}\n  {short}\nnext: 1");
    assert_eq!(redact(&text), "  [redacted]\nnext: 1");
    let text = format!("KEY=\"-----BEGIN PRIVATE KEY----- {line} {short}");
    assert_eq!(redact(&text), "KEY=\"[redacted]");
    // Prose after a header is not a body.
    let text = "the -----BEGIN PRIVATE KEY----- line starts it";
    assert_eq!(redact(text), "the [redacted] line starts it");
}

#[test]
fn legacy_and_rotated_slack_tokens_are_cut() {
    for secret in [
        fake("xoxb-4159162725-", "abcdEFGH", 3),
        fake("xoxe.xoxp-1-", "Mi0yLTE5Nz", 12),
        fake("xoxe.xoxb-1-", "Mi0yLTE5Nz", 12),
        fake("xoxe-1-", "My1xTj3Q", 10),
    ] {
        let text = format!("env: X={secret} done");
        assert_eq!(redact(&text), "env: X=[redacted] done", "{secret}");
    }
    for text in [
        "git checkout xoxb-2-migration-of-legacy-tokens",
        "xoxe-1-refresh-handling",
        "xoxb-12-tokens",
    ] {
        assert!(untouched(text), "{text}");
    }
}

#[test]
fn authorization_in_code_is_a_header() {
    let basic = "dXNlcjpwYXNz";
    for (before, after) in [
        ("headers[\"Authorization\"] = \"", "\""),
        ("req.header(\"Authorization\", \"", "\")"),
        (".header(AUTHORIZATION, \"", "\")"),
        ("{'authorization': '", "'}"),
    ] {
        let text = format!("{before}Basic {basic}{after}");
        assert_eq!(redact(&text), format!("{before}Basic [redacted]{after}"));
    }
    // A bearer token without digits is still a token in a header.
    let t = "abcdefghijKLMNOPqrstuvwx-yz";
    let text = format!(".header(\"Authorization\", \"Bearer {t}\")");
    assert_eq!(
        redact(&text),
        ".header(\"Authorization\", \"Bearer [redacted]\")"
    );
    let text = format!("Authorization: Bearer {t}");
    assert_eq!(redact(&text), "Authorization: Bearer [redacted]");
    // In prose it is not.
    for text in [
        "authorization Basic Requirements",
        "the Bearer abcdefghijKLMNOPqrstuvwx-yz is prose",
        "see authorization; Bearer authentication/authorization-flow",
    ] {
        assert!(untouched(text), "{text}");
    }
}

#[test]
fn prose_between_key_markers_is_not_a_body() {
    let text = "wrap it between -----BEGIN PRIVATE KEY----- and -----END PRIVATE KEY----- markers";
    assert_eq!(
        redact(text),
        "wrap it between [redacted] and -----END PRIVATE KEY----- markers"
    );
}

#[test]
fn a_short_line_after_a_header_is_not_a_body() {
    for (text, want) in [
        (
            "-----BEGIN PRIVATE KEY-----\n> thanks\nok",
            "[redacted]\n> thanks\nok",
        ),
        (
            "-----BEGIN PRIVATE KEY-----\n- rotate\nok",
            "[redacted]\n- rotate\nok",
        ),
        (
            "-----BEGIN PRIVATE KEY-----\n    done\nok",
            "[redacted]\n    done\nok",
        ),
        (
            " let h = 1;\n-// -----BEGIN PRIVATE KEY-----\n-foo\n+bar\n",
            " let h = 1;\n-// [redacted]\n-foo\n+bar\n",
        ),
        (
            "+const B: &str = -----BEGIN PRIVATE KEY-----\n+    + 1\n",
            "+const B: &str = [redacted]\n+    + 1\n",
        ),
    ] {
        assert_eq!(redact(text), want, "{text}");
    }
}

#[test]
fn a_one_line_key_ends_at_a_closing_quote() {
    let short = "AB12cd==";
    for (text, want) in [
        (
            format!("K=\"-----BEGIN PRIVATE KEY----- {BODY} {BODY} {short}\" rest"),
            "K=\"[redacted]\" rest",
        ),
        (
            format!("K=\"-----BEGIN PRIVATE KEY----- {BODY} {BODY}\" rest"),
            "K=\"[redacted]\" rest",
        ),
        (
            format!("K='-----BEGIN PRIVATE KEY----- {BODY} {short}' rest"),
            "K='[redacted]' rest",
        ),
        (
            format!("K=-----BEGIN PRIVATE KEY----- {BODY} {short} and more"),
            "K=[redacted] and more",
        ),
        (
            format!("{{\"k\":\"-----BEGIN PRIVATE KEY-----\\n{BODY}\\n{short}\"}}"),
            "{\"k\":\"[redacted]\"}",
        ),
        (
            format!("{{\"k\":\"-----BEGIN PRIVATE KEY----- {BODY} {short}\\\" x\"}}"),
            "{\"k\":\"[redacted]\\\" x\"}",
        ),
    ] {
        assert_eq!(redact(&text), want, "{text}");
    }
    // Prose after a full line stays.
    let text = format!("K=-----BEGIN PRIVATE KEY----- {BODY} and more");
    assert_eq!(redact(&text), "K=[redacted] and more");
}

#[test]
fn a_deeply_indented_key_without_end_is_cut() {
    let pad = " ".repeat(40);
    let text = format!("{pad}-----BEGIN PRIVATE KEY-----\n{pad}{BODY}\n{pad}AB12cd==\nok");
    assert_eq!(redact(&text), format!("{pad}[redacted]\nok"));
}

#[test]
fn a_slack_token_with_a_suffix_is_cut() {
    let secret = fake("xoxb-123456789012-1234567890123-", "AbCdEfGh", 3);
    let text = format!("X={secret}-prod done");
    assert_eq!(redact(&text), "X=[redacted] done");
    let secret = fake("xoxb-4159162725-", "abcdEFGH", 3);
    let text = format!("X={secret}-staging-2 done");
    assert_eq!(redact(&text), "X=[redacted] done");
}

#[test]
fn a_slack_token_with_no_lower_case_is_cut() {
    let secret = fake("xoxb-123456789012-1234567890123-", "ABCD0123", 3);
    assert_eq!(redact(&format!("X={secret} done")), "X=[redacted] done");
    let secret = fake("xapp-1-A0123BCDE-1234567890123-", "0123ABCD", 4);
    assert_eq!(redact(&format!("X={secret} done")), "X=[redacted] done");
}

#[test]
fn authorization_and_bearer_placeholders_in_prose_and_code_stay() {
    for text in [
        "Authentication, authorization, Basic Requirements",
        "Authorization: Bearer your-access-token-here",
        "Authorization: Bearer Your-Access-Token-Here",
        "authorization=bearer authenticationTokenProvider.get()",
        "authorization: bearer self.tokenProvider.accessToken",
    ] {
        assert!(untouched(text), "{text}");
    }
    // A real token after a quoted name and a comma is still cut.
    let text = "headers.set('authorization', 'Basic dXNlcjpwYXNz')";
    assert_eq!(
        redact(text),
        "headers.set('authorization', 'Basic [redacted]')"
    );
}
