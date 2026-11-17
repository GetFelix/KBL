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
    assert_eq!(c.github.as_ref().unwrap().api, "https://api.github.com");
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
    let a = c.github.unwrap().app.unwrap();
    assert_eq!(a.id, "123");
    assert!(matches!(a.installation, Installation::Id(9)));
    assert!(matches!(
        app(r#","installation_repo":"o/r""#)
            .unwrap()
            .github
            .unwrap()
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

#[test]
fn a_worker_serves_exactly_one_source_with_one_sign_in() {
    let rule = r#"{"name":"r","repo":"acme/FEL","jql":"labels = x","mode":"admit"}"#;
    let jira = |auth: &str| {
        Config::parse(&format!(
            r#"{{"tracker":{{"url":"u","token_env":"T"}},
               "jira":{{"site":"acme","api":"https://a.test"{auth}}},"rules":[{rule}]}}"#
        ))
    };
    assert!(jira(r#","email":"b@a.test","token_env":"J""#).is_ok());
    assert!(jira(r#","oauth_token_file":"/t""#).is_ok());
    // A token needs its email; an OAuth file takes neither.
    assert!(jira(r#","token_env":"J""#).is_err());
    assert!(jira(r#","email":"b@a.test","token_env":"J","oauth_token_file":"/t""#).is_err());
    assert!(jira("").is_err());
    // A Jira rule needs a filter and a repo on its own site.
    let no_jql = r#"{"name":"r","repo":"acme/FEL","mode":"admit"}"#;
    let bad_site = r#"{"name":"r","repo":"other/FEL","jql":"x","mode":"admit"}"#;
    for r in [no_jql, bad_site] {
        assert!(
            Config::parse(&format!(
                r#"{{"tracker":{{"url":"u","token_env":"T"}},"jira":{{"site":"acme","api":"https://a.test","oauth_token_file":"/t"}},"rules":[{r}]}}"#
            ))
            .is_err()
        );
    }
}
