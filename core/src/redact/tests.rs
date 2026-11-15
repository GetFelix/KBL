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

#[test]
fn a_short_first_body_line_with_no_end_is_cut() {
    let first = &BODY[..48];
    let text = format!("-----BEGIN PRIVATE KEY-----\n{first}\nok");
    assert_eq!(redact(&text), "[redacted]\nok");
    // A word, a hash and a sentence are not key material.
    for line in [
        "Done",
        "abcdefabcdefabcdefabcdefabcdefabcdef0123",
        "ThisIsAVeryLongCamelCaseIdentifierName",
    ] {
        let text = format!("-----BEGIN PRIVATE KEY-----\n{line}\nok");
        assert_eq!(redact(&text), format!("[redacted]\n{line}\nok"), "{line}");
    }
}

#[test]
fn an_openssh_body_and_a_blank_line_with_no_end_are_cut() {
    let line = format!("{BODY}AB12cd");
    let text = format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{line}\n{line}\nAB12cd==\nok");
    assert_eq!(redact(&text), "[redacted]\nok");
    let text = format!("-----BEGIN PRIVATE KEY-----\n\n{BODY}\nAB12cd==\nok");
    assert_eq!(redact(&text), "[redacted]\nok");
    // A blank line then prose is still prose.
    let text = "-----BEGIN PRIVATE KEY-----\n\nthanks, done\nok";
    assert_eq!(redact(text), "[redacted]\n\nthanks, done\nok");
}

#[test]
fn double_escaped_and_continued_bodies_are_cut() {
    let short = "AB12cd==";
    for (text, want) in [
        (
            format!(
                "\"-----BEGIN PRIVATE KEY-----\\\\n{BODY}\\\\n{short}\\\\n-----END PRIVATE KEY-----\\\\n\" x"
            ),
            "\"[redacted]\\\\n\" x",
        ),
        (
            format!(
                "\"-----BEGIN PRIVATE KEY-----\\n\\\n    {BODY}\\n\\\n    {short}\\n\\\n    -----END PRIVATE KEY-----\" x"
            ),
            "\"[redacted]\" x",
        ),
    ] {
        assert_eq!(redact(&text), want, "{text}");
    }
}

#[test]
fn a_bearer_in_an_escaped_json_header_is_cut() {
    let text = r#"{\"Authorization\": \"Bearer AbCdEfGhIjKlMnOpQrStUv\"}"#;
    assert_eq!(
        redact(text),
        r#"{\"Authorization\": \"Bearer [redacted]\"}"#
    );
    assert!(untouched(
        r#"\"note\": \"Bearer authentication/authorization-flow\""#
    ));
}

#[test]
fn stuck_aws_ids_and_jwts_are_cut_whole() {
    let id = fake("AKIA", "A1B2", 4);
    assert_eq!(redact(&format!("k={id}{id} z")), "k=[redacted] z");
    assert_eq!(redact(&format!("k={id}{id}{id} z")), "k=[redacted] z");
    // A longer run is a name, not ids.
    assert!(untouched(&format!("feature/{id}EXAMPLE1")));

    let jwt = |sig: &str| format!("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.{sig}");
    let sig = "AbCdEfGhIjKlMnOpQrSt";
    let text = format!("t={}{} z", jwt(sig), jwt(sig));
    assert_eq!(redact(&text), "t=[redacted] z");
    // A JWT followed by a dotted word is still just the JWT.
    let text = format!("t={}.next z", jwt(sig));
    assert_eq!(redact(&text), "t=[redacted].next z");
}

#[test]
fn slack_d_cookies_are_cut() {
    let cookie = fake("xoxd-", "Ab3x%2BQ9Zk", 6);
    assert_eq!(redact(&format!("d={cookie}; x")), "d=[redacted]; x");
    assert!(untouched(
        "branch xoxd-rotate-the-cookie-store-before-release"
    ));
    assert!(untouched("xoxd-short"));
}

#[test]
fn new_shapes_stay_linear() {
    let mb = 10 * 1024 * 1024;
    let inputs = [
        "AKIAA1B2C3D4E5F6A1B2".repeat(mb / 20),
        "xoxd-".repeat(mb / 5),
        format!("xoxd-{}", "a1%2B".repeat(mb / 5)),
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.AbCdEfGhIjKlMnOpQrStAbCdEfeyJ".repeat(mb / 64),
        format!("-----BEGIN PRIVATE KEY-----{}", "\n\n".repeat(mb / 2)),
        format!("-----BEGIN PRIVATE KEY-----{}", "\\\\n".repeat(mb / 3)),
        format!("-----BEGIN PRIVATE KEY-----{}", "\\".repeat(mb)),
        format!("-----BEGIN PRIVATE KEY-----{}", "\\r".repeat(mb / 2)),
        format!("-----BEGIN PRIVATE KEY-----\\n\\\n{}", " ".repeat(mb)),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "A".repeat(mb)),
        "-----BEGIN PRIVATE KEY-----\n\n".repeat(mb / 29),
        "\\\"Authorization\\\": \\\"".repeat(mb / 24),
    ];
    for text in inputs {
        let start = Instant::now();
        let _ = redact(&text);
        let took = start.elapsed();
        assert!(took.as_secs() < 20, "{:?} took {took:?}", &text[..20]);
    }
}

fn mixed(n: usize) -> String {
    "aB3xQ9Zk".repeat(n / 8 + 1)[..n].to_string()
}

#[test]
fn first_line_length_boundaries_with_no_end() {
    let hdr = "-----BEGIN PRIVATE KEY-----";
    // 31 bytes is a word or hash, kept; 32 bytes of mixed classes is a key.
    let l31 = mixed(31);
    assert_eq!(
        redact(&format!("{hdr}\n{l31}\nok")),
        format!("[redacted]\n{l31}\nok")
    );
    assert_eq!(
        redact(&format!("{hdr}\n{}\nok", mixed(32))),
        "[redacted]\nok"
    );
    // Current behaviour: a 32 to 63 byte line with 3 classes is cut. That
    // over-redacts one line after a key header, it never leaks one.
    assert_eq!(
        redact(&format!("{hdr}\n{}\nok", mixed(63))),
        "[redacted]\nok"
    );
}

#[test]
fn full_line_length_boundaries_with_no_end() {
    let hdr = "-----BEGIN PRIVATE KEY-----";
    // 64 to 71 bytes are a body line; a longer mixed first line is a body
    // with its line breaks stripped.
    for n in [64, 65, 70, 71] {
        let text = format!("{hdr}\n{}\nAB12cd==\nok", mixed(n));
        assert_eq!(redact(&text), "[redacted]\nok", "{n}");
    }
    let l72 = mixed(72);
    assert_eq!(redact(&format!("{hdr}\n{l72}\nok")), "[redacted]\nok");
    // A long line of one or two classes is not.
    let hex = "ab12".repeat(20);
    assert_eq!(
        redact(&format!("{hdr}\n{hex}\nok")),
        format!("[redacted]\n{hex}\nok")
    );
    // A diff marker makes a 70 byte line 71 on the page.
    let text = format!("{hdr}\n-{}\n-AB12cd==\nok", mixed(70));
    assert_eq!(redact(&text), "[redacted]\nok");
}

#[test]
fn a_continued_body_with_crlf_is_cut() {
    let text = format!(
        "\"-----BEGIN PRIVATE KEY-----\\n\\\r\n    {BODY}\\n\\\r\n    AB12cd==\\n\\\r\n    -----END PRIVATE KEY-----\" x"
    );
    assert_eq!(redact(&text), "\"[redacted]\" x");
}

#[test]
fn stuck_asia_ids_mixed_ids_and_three_jwts_are_cut_whole() {
    let asia = fake("ASIA", "A1B2", 4);
    let akia = fake("AKIA", "A1B2", 4);
    assert_eq!(redact(&format!("k={asia}{asia} z")), "k=[redacted] z");
    assert_eq!(redact(&format!("k={akia}{asia} z")), "k=[redacted] z");
    assert_eq!(redact(&format!("k={asia}{akia}{asia} z")), "k=[redacted] z");
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.AbCdEfGhIjKlMnOpQrSt";
    assert_eq!(redact(&format!("t={jwt}{jwt}{jwt} z")), "t=[redacted] z");
}

#[test]
fn deeper_escaping_and_continuations_are_cut_whole() {
    let end = "-----END PRIVATE KEY-----";
    let short = "AB12cd==";
    let bs = |n: usize| "\\".repeat(n);
    // Line breaks escaped one to four levels deep (`\n`, `\\n`, `\\\n`, `\\\\n`).
    for depth in 1..=4 {
        let nl = format!("{}n", bs(depth));
        let text = format!("x \"-----BEGIN PRIVATE KEY-----{nl}{BODY}{nl}{short}{nl}{end}{nl}\" y");
        assert_eq!(
            redact(&text),
            format!("x \"[redacted]{nl}\" y"),
            "depth {depth}"
        );
        // The same with no END line.
        let text = format!("x \"-----BEGIN PRIVATE KEY-----{nl}{BODY}{nl}{short}{nl}\" y");
        assert!(!redact(&text).contains(BODY), "depth {depth} no end");
    }
    // A Rust `\n\` continuation, plain, JSON-encoded, and with CRLF.
    for cont in [
        "\\n\\\n    ".to_string(),
        "\\\\n\\\\\\n    ".to_string(),
        "\\r\\n\\\n    ".to_string(),
        "\\\\r\\\\n\\\\\\n    ".to_string(),
        "\\n\\\r\n    ".to_string(),
        "\\\n    ".to_string(),
    ] {
        let text = format!("\"-----BEGIN PRIVATE KEY-----{cont}{BODY}{cont}{short}{cont}{end}\" x");
        assert_eq!(redact(&text), "\"[redacted]\" x", "{cont:?}");
    }
    // Five levels of escaping is sixteen backslashes.
    let nl = format!("{}n", bs(16));
    let text = format!("-----BEGIN PRIVATE KEY-----{nl}{BODY}{nl}{short}{nl}{end}ok");
    assert_eq!(redact(&text), "[redacted]ok");
}

#[test]
fn slack_c_tokens_are_cut() {
    let tok = fake("xoxc-", "1234567890-", 1) + &fake("", "Ab3x9Zk2", 8);
    assert_eq!(redact(&format!("c={tok} z")), "c=[redacted] z");
    assert!(untouched("xoxc-short"));
}

#[test]
fn a_key_with_its_line_breaks_stripped_is_cut_whole() {
    let body = format!("{BODY}{BODY}{BODY}AB12cd==");
    let text = format!("k=-----BEGIN PRIVATE KEY-----{body}-----END PRIVATE KEY----- ok");
    assert_eq!(redact(&text), "k=[redacted] ok");
    // Cut off before its END.
    let text = format!("k=-----BEGIN PRIVATE KEY-----{body} ok");
    assert_eq!(redact(&text), "k=[redacted] ok");
    // A word stuck to a header is not a body.
    assert_eq!(
        redact("-----BEGIN PRIVATE KEY-----markers in prose"),
        "[redacted]markers in prose"
    );
}

#[test]
fn pgp_and_armored_keys_are_cut_whole() {
    let pgp = "PGP PRIVATE KEY BLOCK-----";
    let text = format!(
        "a\n-----BEGIN {pgp}\nComment: work laptop\n\n{BODY}\nAB12cd==\n=Xy9Z\n-----END {pgp}\nb"
    );
    assert_eq!(redact(&text), "a\n[redacted]\nb");
    // With no END the header, armor and body go.
    let text = format!("-----BEGIN {pgp}\nVersion: GnuPG v2\n\n{BODY}\n{BODY}\nAB12cd==\nok");
    assert_eq!(redact(&text), "[redacted]\nok");
    // Legacy encrypted PEM.
    let text = format!(
        "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,A1B2C3D4\n\n{BODY}\nAB12cd==\nok"
    );
    assert_eq!(redact(&text), "[redacted]\nok");
    // A public key stays, and a header line then prose loses only the header.
    assert!(untouched(&format!(
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n{BODY}\n-----END PGP PUBLIC KEY BLOCK-----"
    )));
    let text = "-----BEGIN RSA PRIVATE KEY-----\nNote: rotate it\n\nthanks\nok";
    assert_eq!(redact(text), "[redacted]\nNote: rotate it\n\nthanks\nok");
}

#[test]
fn unicode_and_url_encoded_line_breaks_are_followed() {
    let end = "-----END PRIVATE KEY-----";
    for nl in [
        "\\u000a",
        "\\u000A",
        "\\u000d\\u000a",
        "\\\\u000a",
        "%0A",
        "%0a",
        "%0D%0A",
        "%0d%0a",
    ] {
        let text = format!("x \"-----BEGIN PRIVATE KEY-----{nl}{BODY}{nl}AB12cd=={nl}{end}\" y");
        assert_eq!(redact(&text), "x \"[redacted]\" y", "{nl}");
        let text = format!("x \"-----BEGIN PRIVATE KEY-----{nl}{BODY}{nl}AB12cd==\" y");
        assert!(!redact(&text).contains(BODY), "{nl} no end");
    }
}

#[test]
fn url_encoded_lead_ins_start_a_secret() {
    let gh = fake("ghp_", "A1", 18);
    assert_eq!(redact(&format!("q=x%20{gh}&b")), "q=x%20[redacted]&b");
    let cookie = fake("xoxd-", "Ab3x%2BQ9Zk", 6);
    assert_eq!(
        redact(&format!("Cookie: d%3D{cookie}; x")),
        "Cookie: d%3D[redacted]; x"
    );
    // A word before a prefix still is not a lead-in.
    assert!(untouched(&format!("x{gh}")));
}

#[test]
fn aws_ids_joined_by_one_byte_are_cut_whole() {
    let akia = fake("AKIA", "A1B2", 4);
    let asia = fake("ASIA", "A1B2", 4);
    assert_eq!(redact(&format!("k={akia}x{akia} z")), "k=[redacted] z");
    assert_eq!(
        redact(&format!("k={akia}x{asia}x{akia} z")),
        "k=[redacted] z"
    );
    assert_eq!(redact(&format!("k={akia}{akia}x z")), "k=[redacted] z");
    // An id then a word is still a name.
    assert!(untouched(&format!("k={akia}xEXAMPLEEXAMPLE1234 z")));
}

#[test]
fn encoded_breaks_armor_and_stripped_keys_stay_linear() {
    let mb = 10 * 1024 * 1024;
    let inputs = [
        format!("-----BEGIN PRIVATE KEY-----{}", "A".repeat(mb)),
        "-----BEGIN PRIVATE KEY-----AAAA".repeat(mb / 31),
        format!(
            "-----BEGIN PRIVATE KEY-----{}-----END PRIVATE KEY-----",
            "A".repeat(mb)
        ),
        "-----BEGIN PGP PRIVATE KEY BLOCK-----".repeat(mb / 37),
        "-----BEGIN PRIVATE KEY-----\nA: b\n".repeat(mb / 33),
        format!("-----BEGIN PRIVATE KEY-----\nA: {}", "\\".repeat(mb)),
        format!("-----BEGIN PRIVATE KEY-----\nA: {}", "%".repeat(mb)),
        format!("-----BEGIN PRIVATE KEY-----{}", "\\u000a".repeat(mb / 6)),
        format!("-----BEGIN PRIVATE KEY-----{}", "\\u000d".repeat(mb / 6)),
        format!("-----BEGIN PRIVATE KEY-----{}", "%0D%0A".repeat(mb / 6)),
        "%20ghp_".repeat(mb / 7),
        "AKIAA1B2C3D4E5F6A1B2x".repeat(mb / 21),
        format!("-----BEGIN PRIVATE KEY-----{}", "%2B".repeat(mb / 3)),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "%2Ba".repeat(mb / 4)),
        format!("-----BEGIN PRIVATE KEY-----\nA: {}", "\\x".repeat(mb / 2)),
        "+-----BEGIN PRIVATE KEY-----\n+A: b\n+".repeat(mb / 38),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "ab".repeat(mb / 2)),
    ];
    for text in inputs {
        let start = Instant::now();
        let _ = redact(&text);
        let took = start.elapsed();
        assert!(took.as_secs() < 20, "{:?} took {took:?}", &text[..20]);
    }
}

#[test]
fn reviewed_variants_of_the_new_shapes_are_cut() {
    let end = "-----END PRIVATE KEY-----";
    // Armor values with `%`, a backslash or non-ASCII.
    for comment in ["100% offline backup", "C:\\keys\\me.asc", "Jos\u{e9}"] {
        let pgp = "PGP PRIVATE KEY BLOCK-----";
        let text =
            format!("-----BEGIN {pgp}\nComment: {comment}\n\n{BODY}\n=Xy9Z\n-----END {pgp}\nb");
        assert_eq!(redact(&text), "[redacted]\nb", "{comment}");
    }
    // An encrypted key, and a PGP key with no END, in an added diff hunk.
    let text = format!(
        "+-----BEGIN RSA PRIVATE KEY-----\n+Proc-Type: 4,ENCRYPTED\n+DEK-Info: AES-128-CBC,A1B2\n+\n+{BODY}\n+AB12cd==\n+-----END RSA PRIVATE KEY-----\n fn f() {{}}"
    );
    assert_eq!(redact(&text), "+[redacted]\n fn f() {}");
    let text = format!(
        "+-----BEGIN PGP PRIVATE KEY BLOCK-----\n+\n+{BODY}\n+{BODY}\n+AB12cd==\n fn f() {{}}"
    );
    assert_eq!(redact(&text), "+[redacted]\n fn f() {}");
    // A URL-encoded body with `%2B`, `%2F` and `%3D`, with and without END.
    let line = "MIIEq9Zx%2BIIEq9Zx%2FIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9Zx";
    let text = format!("k=-----BEGIN PRIVATE KEY-----%0A{line}%0AAB12cd%3D%3D%0A{end}&x=1");
    assert_eq!(redact(&text), "k=[redacted]&x=1");
    let text = format!("k=-----BEGIN PRIVATE KEY-----%0A{line}%0A{line}%0AAB12cd%3D%3D&x=1");
    assert!(!redact(&text).contains("Zx%2B"), "{}", redact(&text));
    // A stripped body after a line break or a space, cut off before END.
    let body = format!("{BODY}{BODY}{BODY}AB12");
    for gap in ["\n", " ", "\\n"] {
        let text = format!("\"-----BEGIN PRIVATE KEY-----{gap}{body}");
        assert_eq!(redact(&text), "\"[redacted]", "{gap:?}");
    }
}

#[test]
fn url_encoded_key_headers_are_cut() {
    let line = "MIIEq9Zx%2BIIEq9Zx%2FIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9Zx";
    for sp in ["%20", "+"] {
        let begin = format!("-----BEGIN{sp}RSA{sp}PRIVATE{sp}KEY-----");
        let end = format!("-----END{sp}RSA{sp}PRIVATE{sp}KEY-----");
        let text = format!("k={begin}%0A{line}%0AAB12cd%3D%3D%0A{end}&x=1");
        assert_eq!(redact(&text), "k=[redacted]&x=1", "{sp}");
        let text = format!("k={begin}%0A{line}%0A{line}%0AAB12cd%3D%3D&x=1");
        assert!(!redact(&text).contains("Zx%2B"), "{sp} no end");
        assert_eq!(redact(&format!("q={begin}&x")), "q=[redacted]&x", "{sp}");
        let pgp = format!("-----BEGIN{sp}PGP{sp}PRIVATE{sp}KEY{sp}BLOCK-----");
        assert_eq!(redact(&format!("q={pgp}&x")), "q=[redacted]&x", "{sp}");
    }
    // A header and an END encoded differently are one key.
    let text =
        format!("-----BEGIN%20PRIVATE%20KEY-----\n{BODY}\nAB12cd==\n-----END PRIVATE KEY-----\nok");
    assert_eq!(redact(&text), "[redacted]\nok");
}

#[test]
fn url_encoded_look_alikes_stay() {
    for text in [
        "-----BEGIN+PUBLIC+KEY-----",
        "-----BEGIN%20CERTIFICATE-----",
        "a+-----BEGIN+-----+b",
        "-----BEGIN%2PRIVATE%20KEY-----",
        "-----BEGIN__PRIVATE_KEY-----",
        "q=-----BEGIN%20PRIVATE%20KEYS",
    ] {
        assert!(untouched(text), "{text}");
    }
}

/// A full body line with `/` in it, escaped as JSON escapes it.
fn slashed(escape: &str) -> String {
    "MIIEq9Zx/IIEq9Zx+IIEq9Zx/IIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9Zx".replace('/', escape)
}

#[test]
fn slash_escaped_bodies_are_cut() {
    // JSON, and JSON inside JSON.
    for (esc, nl) in [("\\/", "\\n"), ("\\\\\\/", "\\\\n")] {
        let line = slashed(esc);
        let text = format!(
            "{{\"k\":\"-----BEGIN PRIVATE KEY-----{nl}{line}{nl}AB12{esc}d=={nl}-----END PRIVATE KEY-----{nl}\"}}"
        );
        assert_eq!(
            redact(&text),
            format!("{{\"k\":\"[redacted]{nl}\"}}"),
            "{esc}"
        );
        let text = format!(
            "{{\"k\":\"-----BEGIN PRIVATE KEY-----{nl}{line}{nl}{line}{nl}AB12{esc}d==\"}}"
        );
        assert!(
            !redact(&text).contains("IIEq9Zx"),
            "{esc} no end: {}",
            redact(&text)
        );
        // A stripped body.
        let text = format!("\"-----BEGIN PRIVATE KEY-----{line}{line}{line}AB12\"");
        assert_eq!(redact(&text), "\"[redacted]\"", "{esc} stripped");
    }
}

#[test]
fn escaped_slashes_after_a_header_that_are_not_a_body_stay() {
    let text = "\"-----BEGIN PRIVATE KEY-----\\nsrc\\/pem.rs\\nok\"";
    assert_eq!(redact(text), "\"[redacted]\\nsrc\\/pem.rs\\nok\"");
    // An even run of backslashes is a backslash, then a `/`.
    let line = slashed("\\\\/");
    let text = format!("\"-----BEGIN PRIVATE KEY-----\\n{line}\\nok\"");
    assert!(redact(&text).contains(&line), "{}", redact(&text));
}

#[test]
fn long_pgp_armor_is_cut_whole() {
    let pgp = "PGP PRIVATE KEY BLOCK-----";
    for n in [17, 40, 400] {
        let armor: String = (0..n).map(|k| format!("Comment: note {k}\n")).collect();
        let text =
            format!("a\n-----BEGIN {pgp}\n{armor}\n{BODY}\nAB12cd==\n=Xy9Z\n-----END {pgp}\nb");
        assert_eq!(redact(&text), "a\n[redacted]\nb", "{n}");
        let text = format!("-----BEGIN {pgp}\n{armor}\n{BODY}\n{BODY}\nAB12cd==\nok");
        assert_eq!(redact(&text), "[redacted]\nok", "{n} no end");
    }
}

#[test]
fn long_header_like_lines_then_prose_lose_only_the_header() {
    let lines: String = (0..30).map(|k| format!("Field-{k}: value\n")).collect();
    let text = format!("-----BEGIN RSA PRIVATE KEY-----\n{lines}\nthanks\nok");
    assert_eq!(redact(&text), format!("[redacted]\n{lines}\nthanks\nok"));
    // A key header inside an armor value ends the armor there.
    let text = format!(
        "-----BEGIN PRIVATE KEY-----\nA: b\nC: -----BEGIN PRIVATE KEY-----\nD: e\n\n{BODY}\nAB12cd==\nok"
    );
    assert_eq!(redact(&text), "[redacted]\nA: b\nC: [redacted]\nok");
}

#[test]
fn encoded_headers_escaped_slashes_and_long_armor_stay_linear() {
    let mb = 10 * 1024 * 1024;
    let inputs = [
        "-----BEGIN%20PRIVATE%20KEY-----".repeat(mb / 31),
        "-----BEGIN+PRIVATE+KEY-----".repeat(mb / 27),
        format!("-----BEGIN%20{}", "%20".repeat(mb / 3)),
        format!("-----BEGIN+{}", "+".repeat(mb)),
        "-----BEGIN%20PRIVATE%20KEY-----%0A".repeat(mb / 34),
        format!("-----BEGIN PRIVATE KEY-----{}", "\\/".repeat(mb / 2)),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "\\/".repeat(mb / 2)),
        format!("-----BEGIN PRIVATE KEY-----\n{}/", "\\".repeat(mb)),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "a\\/".repeat(mb / 3)),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "A: b\n".repeat(mb / 5)),
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}",
            "A: -----BEGIN PRIVATE KEY-----\n".repeat(mb / 31)
        ),
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}",
            "A: x -----BEGIN PRIVATE KEY----- -----BEGIN PRIVATE KEY-----\n".repeat(mb / 62)
        ),
        "-----BEGIN PRIVATE KEY-----\nA: b\nA: b\nA: b\n\n".repeat(mb / 44),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "A: b\n".repeat(mb / 10))
            + &"-----END PRIVATE KEY-----".repeat(mb / 50),
    ];
    for text in inputs {
        let start = Instant::now();
        let _ = redact(&text);
        let took = start.elapsed();
        assert!(took.as_secs() < 20, "{:?} took {took:?}", &text[..20]);
    }
}

#[test]
fn a_url_encoded_key_on_one_line_is_cut() {
    let line = "MIIEq9Zx%2BIIEq9Zx%2FIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9ZxMIIEq9Zx";
    let begin = "-----BEGIN%20PRIVATE%20KEY-----";
    let end = "-----END%20PRIVATE%20KEY-----";
    let text = format!("k={begin}%20{line}%20{line}%20AB12cd%3D%3D%20{end}&x");
    assert_eq!(redact(&text), "k=[redacted]&x");
    let text = format!("k={begin}%20{line}%20{line}%20AB12cd%3D%3D&x");
    assert!(!redact(&text).contains("Zx%2B"), "{}", redact(&text));
    // Words after an encoded header are not a body.
    assert_eq!(
        redact(&format!("q={begin}%20and%20more%20text")),
        "q=[redacted]%20and%20more%20text"
    );
}

#[test]
fn unicode_escaped_base64_bytes_are_read_in_a_body() {
    for (plus, slash, eq, nl) in [
        ("\\u002b", "\\u002f", "\\u003d", "\\n"),
        ("\\u002B", "\\u002F", "\\u003D", "\\n"),
        ("\\\\u002b", "\\\\u002f", "\\\\u003d", "\\\\n"),
    ] {
        let line = slashed("/").replace('/', slash).replace('+', plus);
        let text = format!(
            "{{\"k\":\"-----BEGIN PRIVATE KEY-----{nl}{line}{nl}AB12cd{eq}{eq}{nl}-----END PRIVATE KEY-----{nl}\"}}"
        );
        assert_eq!(
            redact(&text),
            format!("{{\"k\":\"[redacted]{nl}\"}}"),
            "{plus}"
        );
        let text = format!("{{\"k\":\"-----BEGIN PRIVATE KEY-----{nl}{line}{nl}{line}{nl}AB12\"}}");
        assert!(!redact(&text).contains("IIEq9Zx"), "{plus} no end");
    }
    // Other `\u` escapes still end a run.
    let text = "\"-----BEGIN PRIVATE KEY-----\\nsrc\\u0020pem.rs\\nok\"";
    assert_eq!(redact(text), "\"[redacted]\\nsrc\\u0020pem.rs\\nok\"");
}

#[test]
fn a_header_stuck_to_a_word_in_armor_does_not_end_it() {
    let pgp = "PGP PRIVATE KEY BLOCK-----";
    let text = format!(
        "-----BEGIN {pgp}\nComment: see x-----BEGIN PRIVATE KEY-----\nVersion: 1\n\n{BODY}\nAB12cd==\n-----END {pgp}\nok"
    );
    assert_eq!(redact(&text), "[redacted]\nok");
}

#[test]
fn encoded_gaps_and_unicode_escapes_stay_linear() {
    let mb = 10 * 1024 * 1024;
    let inputs = [
        format!("-----BEGIN PRIVATE KEY-----{}", "%20".repeat(mb / 3)),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "AB%20".repeat(mb / 5)),
        format!("-----BEGIN PRIVATE KEY-----\n{}", "\\u002b".repeat(mb / 6)),
        format!("-----BEGIN PRIVATE KEY-----\n{}u002", "\\".repeat(mb)),
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}",
            "A: x-----BEGIN PRIVATE KEY-----\n".repeat(mb / 32)
        ),
    ];
    for text in inputs {
        let start = Instant::now();
        let _ = redact(&text);
        let took = start.elapsed();
        assert!(took.as_secs() < 20, "{:?} took {took:?}", &text[..20]);
    }
}
