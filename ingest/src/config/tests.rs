use super::*;

fn parse(rules: &str, github: &str) -> Result<Config> {
    Config::parse(&format!(
        r#"{{"tracker":{{"url":"http://t","token_env":"T"}},"github":{github},"rules":{rules}}}"#
    ))
}

const RULE: &str = r#"{"name":"a","repo":"GetFelix/felix","mode":"admit"}"#;
const GH: &str = r#"{"token_env":"G"}"#;

#[test]
fn a_config_has_defaults_and_lists_the_repos_once() {
    let c = parse(&format!("[{RULE},{}]", RULE.replace("\"a\"", "\"b\"")), GH).unwrap();
    assert_eq!(c.poll_seconds, 60);
    assert_eq!(c.github.api, "https://api.github.com");
    assert_eq!(c.rules[0].default_tier, 2);
    assert_eq!(c.repos(), ["GetFelix/felix"]);
}

#[test]
fn a_bad_config_is_refused_with_the_reason() {
    for (rules, gh, why) in [
        ("[]", GH, "no intake rules"),
        (&format!("[{RULE},{RULE}]"), GH, "listed twice"),
        (&format!("[{RULE}]"), "{}", "token"),
        (
            r#"[{"name":"a","repo":"felix","mode":"admit"}]"#,
            GH,
            "owner/name",
        ),
        (
            r#"[{"name":"a","repo":"o/r","mode":"admit","default_tier":4}]"#,
            GH,
            "0 to 3",
        ),
        (
            r#"[{"name":"a","repo":"o/r","mode":"admit","typo":1}]"#,
            GH,
            "typo",
        ),
        (r#"[{"name":"a","repo":"o/r","mode":"maybe"}]"#, GH, "maybe"),
    ] {
        let err = format!("{:#}", parse(rules, gh).unwrap_err());
        assert!(err.contains(why), "{err} should mention {why}");
    }
}

#[test]
fn an_app_names_one_installation_and_a_key_file() {
    let app = |extra: &str| {
        parse(
            &format!("[{RULE}]"),
            &format!(r#"{{"app":{{"id":123,"private_key_file":"/k.pem"{extra}}}}}"#),
        )
    };
    let c = app(r#","installation_id":9"#).unwrap();
    let a = c.github.app.unwrap();
    assert_eq!(a.id, "123");
    assert!(matches!(a.installation, Installation::Id(9)));
    assert!(matches!(
        app(r#","installation_repo":"o/r""#)
            .unwrap()
            .github
            .app
            .unwrap()
            .installation,
        Installation::Repo(_)
    ));
    for bad in [
        "",
        r#","installation_id":9,"installation_org":"o""#,
        r#","installation_repo":"nope""#,
    ] {
        let err = format!("{:#}", app(bad).unwrap_err());
        assert!(err.contains("exactly one of installation_id"), "{err}");
    }
}
