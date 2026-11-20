use super::*;

const BASE: &str = r#"
default_profile = "fake"
[profiles.fake]
kind = "command"
vendor = "fake"
model = "small"
command = ["fake-agent", "--model", "{model}", "--mcp", "{mcp_config_path}", "--in={workdir}", "{worker}"]
"#;

fn refused(extra: &str) -> String {
    let err = Config::parse(&format!("{BASE}{extra}")).unwrap_err();
    format!("{err:#}")
}

#[test]
fn a_profile_parses_with_the_four_placeholders() {
    let c = Config::parse(BASE).unwrap();
    assert_eq!(c.profile("fake").unwrap().kind, Kind::Command);
    assert!(c.profile("fake").unwrap().needs_model());
    assert_eq!(c.top_up_every(), Duration::from_secs(30));
}

#[test]
fn a_template_takes_no_other_placeholder_and_the_program_takes_none() {
    for bad in [
        r#"command = ["x", "{title}"]"#,
        r#"command = ["x", "{model"]"#,
        r#"command = ["x", "model}"]"#,
        r#"command = ["x", "{{model}}"]"#,
        r#"command = ["{worker}"]"#,
        r#"command = []"#,
    ] {
        let text = BASE.replace(
            r#"command = ["fake-agent", "--model", "{model}", "--mcp", "{mcp_config_path}", "--in={workdir}", "{worker}"]"#,
            bad,
        );
        assert!(Config::parse(&text).is_err(), "{bad}");
    }
    assert!(refused("workdir = \"/w/{model}\"").contains("only {worker}"));
}

#[test]
fn no_kbl_variable_or_secret_reaches_a_worker() {
    assert!(refused(r#"pass_env = ["KBL_TRACKER_TOKEN"]"#).contains("not passed"));
    assert!(refused(r#"pass_env = ["kbl_felix_token"]"#).contains("not passed"));
    assert!(refused(r#"set_env = { KBL_AGENT = "x" }"#).contains("not passed"));
    assert!(refused(r#"pass_env = ["A B"]"#).contains("not a variable"));
    let hook = r#"
[profiles.cloud]
kind = "hook"
vendor = "x"
url = "https://hooks.example/start"
hook_token_env = "KBL_TRACKER_TOKEN"
"#;
    assert!(refused(hook).contains("may not be"));
}

#[test]
fn a_hook_is_https_or_loopback() {
    let hook =
        |url: &str| format!("[profiles.cloud]\nkind = \"hook\"\nvendor = \"x\"\nurl = \"{url}\"\n");
    assert!(Config::parse(&format!("{BASE}{}", hook("https://h.example/s"))).is_ok());
    assert!(Config::parse(&format!("{BASE}{}", hook("http://127.0.0.1:9/s"))).is_ok());
    assert!(refused(&hook("http://h.example/s")).contains("https"));
}

#[test]
fn a_model_cannot_look_like_a_flag_or_carry_a_space() {
    for ok in [
        "opus",
        "gpt-5-codex",
        "claude-sonnet-4.5",
        "org/model:tag@1",
    ] {
        check_model(ok).unwrap();
    }
    for bad in [
        "",
        "-x",
        "--dangerous",
        "a b",
        "a;b",
        "a\nb",
        "{worker}",
        &"m".repeat(65),
    ] {
        assert!(check_model(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_first_matching_row_wins_and_no_match_falls_back() {
    let text = format!(
        "{BASE}{}",
        r#"
[profiles.big]
kind = "command"
vendor = "x"
command = ["big", "{model}"]

[[fit]]
tiers = [0, 1]
profile = "big"
model = "opus"

[[fit]]
areas = ["Storage"]
sizes = ["L"]
profile = "big"
model = "deep"

[[fit]]
needs = ["gpu"]
kinds = ["fix"]
profile = "big"
"#
    );
    let c = Config::parse(&text).unwrap();
    let f = |tier, area: Option<&str>, size: Option<&str>| Fields {
        tier: Some(tier),
        area: area.map(str::to_owned),
        size: size.map(str::to_owned),
        ..Fields::default()
    };
    assert_eq!(
        c.fit(Some(&f(1, Some("storage"), Some("L")))),
        Some(("big", Some("opus")))
    );
    assert_eq!(
        c.fit(Some(&f(2, Some("storage"), Some("L")))),
        Some(("big", Some("deep")))
    );
    assert_eq!(
        c.fit(Some(&f(2, Some("storage"), Some("S")))),
        Some(("fake", None))
    );
    let gpu_fix = Fields {
        tier: Some(3),
        kind: Some("fix".into()),
        needs: vec!["gpu".into()],
        ..Fields::default()
    };
    assert_eq!(c.fit(Some(&gpu_fix)), Some(("big", None)));
    let gpu_docs = Fields {
        kind: Some("triage".into()),
        ..gpu_fix
    };
    assert_eq!(c.fit(Some(&gpu_docs)), Some(("fake", None)));
}

#[test]
fn a_fit_row_names_a_profile_that_exists() {
    assert!(refused("[[fit]]\nprofile = \"nope\"\n").contains("fit row 1"));
}

#[test]
fn peeked_fields_are_read_from_the_peek_reply() {
    let item = serde_json::json!({
        "work": "fix", "tier": 1, "type": "bug", "area": "storage", "size": "M", "needs": ["gpu"]
    });
    assert_eq!(
        Fields::of_peeked(&item),
        Fields {
            tier: Some(1),
            kind: Some("fix".into()),
            item_type: Some("bug".into()),
            area: Some("storage".into()),
            size: Some("M".into()),
            needs: vec!["gpu".into()],
        }
    );
}

#[test]
fn a_git_identity_becomes_the_four_git_variables_and_set_env_wins() {
    let ident = r#"git_identity = { name = "Bot", email = "bot@example.com" }"#;
    let c = Config::parse(&format!("{BASE}{ident}")).unwrap();
    let env = c.profile("fake").unwrap().fixed_env();
    for k in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
        assert_eq!(env[k], "bot@example.com");
    }
    for k in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
        assert_eq!(env[k], "Bot");
    }
    let both = format!("{BASE}{ident}\nset_env = {{ GIT_AUTHOR_EMAIL = \"other@example.com\" }}");
    let env = Config::parse(&both)
        .unwrap()
        .profile("fake")
        .unwrap()
        .fixed_env();
    assert_eq!(env["GIT_AUTHOR_EMAIL"], "other@example.com");
    assert_eq!(env["GIT_COMMITTER_EMAIL"], "bot@example.com");
}

#[test]
fn a_git_identity_with_a_bad_email_or_name_is_refused() {
    for email in [
        "nobody",
        "a@b",
        "@example.com",
        "a b@example.com",
        "a@@example.com",
        "",
    ] {
        let text = format!("git_identity = {{ name = \"Bot\", email = \"{email}\" }}");
        assert!(refused(&text).contains("not an email"), "{email}");
    }
    assert!(refused(r#"git_identity = { name = " ", email = "a@b.co" }"#).contains("name"));
}
