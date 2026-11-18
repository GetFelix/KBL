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
    assert_eq!(c.poll_seconds, 300);
    assert_eq!(c.reconcile_hours, 24);
    assert_eq!(c.budget_share, 0.5);
    assert_eq!(c.budget_per_hour, None);
    assert_eq!(c.github.as_ref().unwrap().api, "https://api.github.com");
    assert_eq!(c.rules[0].default_tier, 2);
    assert_eq!(c.repos(), ["GetFelix/felix"]);
}

#[test]
fn reconcile_every_is_refused_with_its_replacement() {
    let err = Config::parse(&format!(
        r#"{{"tracker":{{"url":"http://t","token_env":"T"}},"github":{GH},"poll_seconds":600,"reconcile_every":12,"rules":[{RULE}]}}"#
    ))
    .unwrap_err()
    .to_string();
    assert!(err.contains("reconcile_hours"), "{err}");
    assert!(
        err.contains("12 polls of 600s is about reconcile_hours: 2"),
        "{err}"
    );
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

#[test]
fn a_jira_filter_may_not_order_itself() {
    let parse = |jql: &str| {
        Config::parse(&format!(
            r#"{{"tracker":{{"url":"u","token_env":"T"}},"jira":{{"site":"acme","api":"https://a.test","oauth_token_file":"/t"}},
               "rules":[{{"name":"r","repo":"acme/FEL","jql":{jql:?},"mode":"admit"}}]}}"#
        ))
    };
    for bad in [
        "labels = x order by created",
        "labels = x ORDER  BY created DESC",
    ] {
        let err = format!("{:#}", parse(bad).unwrap_err());
        assert!(err.contains("ORDER BY"), "{err}");
    }
    assert!(parse(r#"summary ~ "order by" AND labels = x"#).is_ok());
    assert!(parse("labels = x").is_ok());
}

/// The starter configs `kbl-tool source init` writes, filled in the way it
/// fills them, must be configs the worker accepts.
#[test]
fn the_starter_configs_of_kbl_tool_parse() {
    let fill = |name: &str| {
        let path = format!(
            "{}/../tool/src/source/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut text = std::fs::read_to_string(&path).expect(&path);
        text = text.replace("{{cap}}", "5");
        let values = [
            ("repo", "acme/FEL"),
            ("site", "acme"),
            ("api", "https://acme.atlassian.net"),
            ("email", "kbl@acme.example"),
            ("token_env", "SOURCE_TOKEN"),
            ("tracker_url", "http://127.0.0.1:8790"),
            ("label", "agent-ok"),
            ("triage", "run"),
            ("jql", "labels = \"agent-ok\""),
            ("mode", "admit"),
            ("target_repo", "GetFelix/felix"),
        ];
        for (k, v) in values {
            text = text.replace(&format!("{{{{{k}}}}}"), &serde_json::to_string(v).unwrap());
        }
        text
    };
    let github = fill("github").replace("acme/FEL", "GetFelix/felix");
    Config::parse(&github).unwrap();
    let jira = Config::parse(&fill("jira")).unwrap();
    assert!(jira.jira.is_some());
}

fn ado(block: &str, rule: &str) -> Result<Config> {
    Config::parse(&format!(
        r#"{{"tracker":{{"url":"u","token_env":"T"}},"ado":{block},"rules":[{rule}]}}"#
    ))
}

const ADO_RULE: &str = r#"{"name":"r","repo":"contoso/Platform","mode":"admit"}"#;

#[test]
fn an_ado_worker_signs_in_one_way_and_reads_projects_of_its_organization() {
    let pat = r#"{"organization":"contoso","pat_env":"P"}"#;
    let entra = |secret: &str| {
        format!(
            r#"{{"organization":"contoso","entra":{{"tenant_id":"t.example","client_id":"c"{secret}}}}}"#
        )
    };
    let c = ado(pat, ADO_RULE).unwrap();
    assert!(c.rules[0].ado);
    assert_eq!(c.ado.unwrap().api, "https://dev.azure.com");
    assert!(ado(r#"{"organization":"contoso","pat_file":"/p"}"#, ADO_RULE).is_ok());
    assert!(ado(&entra(r#","client_secret_env":"S""#), ADO_RULE).is_ok());
    assert!(ado(&entra(r#","client_secret_file":"/s""#), ADO_RULE).is_ok());
    // Exactly one sign-in, and exactly one place for the client secret.
    assert!(ado(&entra(""), ADO_RULE).is_err());
    assert!(
        ado(
            &entra(r#","client_secret_env":"S","client_secret_file":"/s""#),
            ADO_RULE
        )
        .is_err()
    );
    assert!(ado(r#"{"organization":"contoso"}"#, ADO_RULE).is_err());
    assert!(
        ado(
            r#"{"organization":"contoso","pat_env":"P","entra":{"tenant_id":"t","client_id":"c","client_secret_env":"S"}}"#,
            ADO_RULE
        )
        .is_err()
    );
    assert!(
        ado(
            r#"{"organization":"contoso","pat_env":"P","oauth":1}"#,
            ADO_RULE
        )
        .is_err()
    );
    // A tenant is a name, never a path into the token address.
    let bad_tenant = entra(r#","client_secret_env":"S""#).replace("t.example", "x/../y");
    assert!(ado(&bad_tenant, ADO_RULE).is_err());
    // A rule's repo is organization/project; and only one source per worker.
    assert!(
        ado(
            pat,
            r#"{"name":"r","repo":"other/Platform","mode":"admit"}"#
        )
        .is_err()
    );
    assert!(ado(pat, r#"{"name":"r","repo":"contoso/a/b","mode":"admit"}"#).is_err());
    let both = format!(
        r#"{{"tracker":{{"url":"u","token_env":"T"}},"ado":{pat},"github":{{"token_env":"G"}},"rules":[{ADO_RULE}]}}"#
    );
    assert!(Config::parse(&both).is_err());
}

#[test]
fn ado_rule_fields_belong_to_ado_and_a_query_path_stays_inside_queries() {
    let pat = r#"{"organization":"contoso","pat_env":"P"}"#;
    let rule =
        |extra: &str| format!(r#"{{"name":"r","repo":"contoso/Platform","mode":"admit",{extra}}}"#);
    assert!(ado(pat, &rule(r#""ado_query":"Shared Queries/Agent""#)).is_ok());
    assert!(
        ado(
            pat,
            &rule(r#""ado_query":"0b9e1d3e-7c1a-4f0e-9a52-5f2f0c0d1e11""#)
        )
        .is_ok()
    );
    assert!(ado(pat, &rule(r#""area_paths":{"Platform\\Storage":"o/r"},"comment":true,"link_pr":true,"transition_on_pr":"Resolved""#)).is_ok());
    for bad in [
        r#""ado_query":"../Other""#,
        r#""ado_query":"Shared Queries/../../x""#,
        r#""ado_query":" ""#,
        r#""jql":"labels = x""#,
        r#""components":{"a":"o/r"}"#,
        r#""area_paths":{"":"o/r"}"#,
    ] {
        assert!(ado(pat, &rule(bad)).is_err(), "{bad}");
    }
    // The ADO fields are refused under another source.
    let gh = r#"[{"name":"r","repo":"o/r","mode":"admit","ado_query":"Q"}]"#;
    assert!(parse(gh, GH).is_err());
    let gh = r#"[{"name":"r","repo":"o/r","mode":"admit","area_paths":{"a":"o/r"}}]"#;
    assert!(parse(gh, GH).is_err());
}

fn gitlab(block: &str, rule: &str) -> Result<Config> {
    Config::parse(&format!(
        r#"{{"tracker":{{"url":"u","token_env":"T"}},"gitlab":{block},"rules":[{rule}]}}"#
    ))
}

const GL_RULE: &str = r#"{"name":"r","repo":"team/app","mode":"admit"}"#;

#[test]
fn a_gitlab_worker_has_one_token_and_a_safe_address() {
    let c = gitlab(r#"{"token_env":"G"}"#, GL_RULE).unwrap();
    assert!(c.rules[0].gitlab && !c.rules[0].ado);
    assert_eq!(c.gitlab.unwrap().api, "https://gitlab.com");
    assert!(gitlab(r#"{"token_file":"/t"}"#, GL_RULE).is_ok());
    // Exactly one place for the token.
    assert!(gitlab(r#"{}"#, GL_RULE).is_err());
    assert!(gitlab(r#"{"token_env":"G","token_file":"/t"}"#, GL_RULE).is_err());
    // A self-managed instance: https, or http on loopback. Never a plain remote address.
    let api = |a: &str| gitlab(&format!(r#"{{"token_env":"G","api":"{a}"}}"#), GL_RULE);
    assert!(api("https://git.corp.example/gitlab").is_ok());
    assert!(api("http://127.0.0.1:8929").is_ok());
    for bad in [
        "http://git.corp.example",
        "https://u:p@git.corp.example",
        "git.corp.example",
        "https://git.corp.example/?x=1",
    ] {
        let err = api(bad).unwrap_err().to_string();
        assert!(
            err.contains("gitlab.api") && !err.contains("u:p"),
            "{bad}: {err}"
        );
    }
    // Only one source per worker.
    let both = format!(
        r#"{{"tracker":{{"url":"u","token_env":"T"}},"gitlab":{{"token_env":"G"}},"github":{{"token_env":"G"}},"rules":[{GL_RULE}]}}"#
    );
    assert!(Config::parse(&both).is_err());
}

#[test]
fn a_gitlab_rule_names_a_project_and_a_subgroup_project_goes_through_projects() {
    let block = r#"{"token_env":"G","projects":{"team/app":"group/sub/app"}}"#;
    let rule = |repo: &str| format!(r#"{{"name":"r","repo":"{repo}","mode":"admit"}}"#);
    assert!(gitlab(block, &rule("team/app")).is_ok());
    assert!(gitlab(r#"{"token_env":"G"}"#, &rule("group/app")).is_ok());
    // A subgroup path cannot be an issue id, so it needs an alias.
    assert!(gitlab(r#"{"token_env":"G"}"#, &rule("group/sub/app")).is_err());
    assert!(gitlab(r#"{"token_env":"G"}"#, &rule("app")).is_err());
    // An alias is one slash, and the path it names is plain names.
    for bad in [
        r#"{"token_env":"G","projects":{"a/b/c":"x/y"}}"#,
        r#"{"token_env":"G","projects":{"a/b":"x"}}"#,
        r#"{"token_env":"G","projects":{"a/b":"x/../y"}}"#,
        r#"{"token_env":"G","projects":{"a/b":"x/y?z"}}"#,
    ] {
        assert!(gitlab(bad, &rule("a/b")).is_err(), "{bad}");
    }
}

#[test]
fn gitlab_write_back_is_a_comment_and_a_close_or_reopen() {
    let block = r#"{"token_env":"G"}"#;
    let rule = |extra: &str| format!(r#"{{"name":"r","repo":"team/app","mode":"admit",{extra}}}"#);
    assert!(
        gitlab(
            block,
            &rule(r#""comment":true,"transition_on_pr":"reopen","transition_on_merge":"close""#)
        )
        .is_ok()
    );
    for bad in [
        r#""link_pr":true"#,
        r#""transition_on_merge":"Done""#,
        r#""transition_on_pr":"Resolved""#,
        r#""jql":"labels = x""#,
        r#""ado_query":"Q""#,
        r#""area_paths":{"a":"o/r"}"#,
        r#""components":{"a":"o/r"}"#,
    ] {
        assert!(gitlab(block, &rule(bad)).is_err(), "{bad}");
    }
}

#[test]
fn a_rule_may_skip_triage_only_when_it_admits() {
    let rule = |extra: &str| format!(r#"[{{"name":"a","repo":"o/r",{extra}}}]"#);
    let skip = parse(&rule(r#""mode":"admit","triage":"skip""#), GH).unwrap();
    assert_eq!(skip.rules[0].triage, Triage::Skip);
    let plain = parse(&rule(r#""mode":"admit""#), GH).unwrap();
    assert_eq!(plain.rules[0].triage, Triage::Run);
    let err = format!(
        "{:#}",
        parse(&rule(r#""mode":"propose","triage":"skip""#), GH).unwrap_err()
    );
    assert!(err.contains("triage"), "{err}");
    assert!(parse(&rule(r#""mode":"admit","triage":"never""#), GH).is_err());
}

#[test]
fn a_skip_rule_name_must_fit_the_record() {
    let long = "n".repeat(121);
    let rules = format!(r#"[{{"name":"{long}","repo":"o/r","mode":"admit","triage":"skip"}}]"#);
    assert!(parse(&rules, GH).is_err());
    let ok = format!(r#"[{{"name":"{long}","repo":"o/r","mode":"admit"}}]"#);
    assert!(parse(&ok, GH).is_ok());
}
