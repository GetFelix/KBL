use super::*;
use crate::org::{OrgLogged, OrgRecord, Role, TeamName, TokenHash};

fn key(s: &str) -> Key {
    Key::new(s).unwrap()
}
fn team(s: &str) -> TeamName {
    TeamName::new(s).unwrap()
}
fn tok(s: &str) -> TokenHash {
    TokenHash::of(s)
}
fn log(r: OrgRecord) -> OrgLogged {
    OrgLogged::new(1, key("ana"), r)
}
fn create(t: &str) -> OrgRecord {
    OrgRecord::TeamCreated {
        team: team(t),
        display_name: None,
        adopted: false,
    }
}
fn person(p: &str, token: &str) -> OrgRecord {
    OrgRecord::PersonAdded {
        person: key(p),
        token_sha256: tok(token),
        subject: None,
    }
}
fn grant_role(t: &str, p: &str, role: Role) -> OrgRecord {
    OrgRecord::RoleGranted {
        team: Some(team(t)),
        person: key(p),
        role,
        repos: vec![],
    }
}
fn agent(t: &str, a: &str, token: &str) -> OrgRecord {
    OrgRecord::AgentAdded {
        team: team(t),
        agent: key(a),
        token_sha256: tok(token),
    }
}
fn run(state: &mut OrgState, r: OrgRecord) -> Result<Applied, OrgRefusal> {
    state.apply(&log(r))
}
fn base() -> OrgState {
    let mut s = OrgState::default();
    for r in [
        create("payments"),
        create("web"),
        person("ana", "t-ana"),
        person("bo", "t-bo"),
        grant_role("payments", "ana", Role::Admin),
    ] {
        run(&mut s, r).unwrap();
    }
    s
}

#[test]
fn team_names_follow_the_key_rule_and_keep_the_org_namespace() {
    assert!(TeamName::new("payments").is_ok());
    assert!(TeamName::new("kbl").is_err());
    assert!(TeamName::new("").is_err());
    assert!(TeamName::new("a b").is_err());
    assert!(TeamName::new("a".repeat(65)).is_err());
    let bad: Result<OrgRecord, _> = serde_json::from_str(r#"{"kind":"team_created","team":"kbl"}"#);
    assert!(bad.is_err());
}

#[test]
fn a_token_hash_is_64_hex_and_matches_only_its_token() {
    let h = TokenHash::of("secret");
    assert_eq!(h.as_str().len(), 64);
    assert_eq!(
        h.as_str(),
        "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
    );
    assert!(h.matches("secret"));
    assert!(!h.matches("secreT"));
    assert!(TokenHash::parse("ABC").is_err());
    assert!(TokenHash::parse(&"G".repeat(64)).is_err());
    assert!(!format!("{h:?}").contains(h.as_str()));
}

#[test]
fn creating_a_team_twice_changes_nothing() {
    let mut s = OrgState::default();
    assert_eq!(run(&mut s, create("web")), Ok(Applied::Changed));
    assert_eq!(run(&mut s, create("web")), Ok(Applied::Unchanged));
    assert_eq!(s.teams().count(), 1);
}

#[test]
fn a_person_name_and_a_token_are_each_used_once() {
    let mut s = base();
    assert_eq!(
        run(&mut s, person("ana", "other")),
        Err(OrgRefusal::NameTaken)
    );
    assert_eq!(run(&mut s, person("ana", "t-ana")), Ok(Applied::Unchanged));
    assert_eq!(
        run(&mut s, person("cy", "t-bo")),
        Err(OrgRefusal::TokenInUse)
    );
    run(&mut s, agent("web", "w1", "t-w1")).unwrap();
    assert_eq!(
        run(&mut s, person("cy", "t-w1")),
        Err(OrgRefusal::TokenInUse)
    );
    assert_eq!(
        run(&mut s, agent("web", "w2", "t-bo")),
        Err(OrgRefusal::TokenInUse)
    );
}

#[test]
fn an_oidc_subject_belongs_to_one_person() {
    let mut s = base();
    let with = |p: &str, t: &str| OrgRecord::PersonAdded {
        person: key(p),
        token_sha256: tok(t),
        subject: Some("sub-1".into()),
    };
    run(&mut s, with("cy", "t-cy")).unwrap();
    assert_eq!(
        run(&mut s, with("di", "t-di")),
        Err(OrgRefusal::SubjectTaken)
    );
}

#[test]
fn a_role_needs_a_person_and_a_team_and_the_right_scope() {
    let mut s = base();
    assert_eq!(
        run(&mut s, grant_role("payments", "zed", Role::Viewer)),
        Err(OrgRefusal::UnknownPerson)
    );
    assert_eq!(
        run(&mut s, grant_role("nope", "bo", Role::Viewer)),
        Err(OrgRefusal::UnknownTeam)
    );
    assert_eq!(
        run(&mut s, grant_role("payments", "bo", Role::OrgAdmin)),
        Err(OrgRefusal::RoleScope)
    );
    let no_team = |role| OrgRecord::RoleGranted {
        team: None,
        person: key("bo"),
        role,
        repos: vec![],
    };
    assert_eq!(
        run(&mut s, no_team(Role::Admin)),
        Err(OrgRefusal::RoleScope)
    );
    assert_eq!(run(&mut s, no_team(Role::OrgAdmin)), Ok(Applied::Changed));
    assert!(s.is_org_admin(&key("bo")));
    assert_eq!(s.role_in(&team("web"), &key("bo")), Some(Role::OrgAdmin));
}

#[test]
fn a_role_narrowed_to_repos_names_only_the_teams_own_sources() {
    let mut s = base();
    let narrowed = |repo: &str| OrgRecord::RoleGranted {
        team: Some(team("payments")),
        person: key("bo"),
        role: Role::Triager,
        repos: vec![repo.into()],
    };
    assert_eq!(
        run(&mut s, narrowed("github:a/b")),
        Err(OrgRefusal::UnknownSource)
    );
    run(
        &mut s,
        OrgRecord::SourceAdded {
            team: team("web"),
            source: "github:a/b".into(),
        },
    )
    .unwrap();
    assert_eq!(
        run(&mut s, narrowed("github:a/b")),
        Err(OrgRefusal::UnknownSource)
    );
    run(
        &mut s,
        OrgRecord::SourceAdded {
            team: team("payments"),
            source: "github:a/c".into(),
        },
    )
    .unwrap();
    assert_eq!(run(&mut s, narrowed("github:a/c")), Ok(Applied::Changed));
    assert_eq!(
        s.member(&team("payments"), &key("bo")).unwrap().repos,
        vec!["github:a/c".to_string()]
    );
}

#[test]
fn the_last_admin_of_a_team_stays() {
    let mut s = base();
    let revoke = OrgRecord::RoleRevoked {
        team: Some(team("payments")),
        person: key("ana"),
    };
    assert_eq!(run(&mut s, revoke.clone()), Err(OrgRefusal::LastAdmin));
    assert_eq!(
        run(&mut s, grant_role("payments", "ana", Role::Viewer)),
        Err(OrgRefusal::LastAdmin)
    );
    run(&mut s, grant_role("payments", "bo", Role::Admin)).unwrap();
    assert_eq!(run(&mut s, revoke), Ok(Applied::Changed));
    assert_eq!(s.role_in(&team("payments"), &key("ana")), None);
}

#[test]
fn the_last_org_admin_stays_and_revoking_a_non_member_is_a_no_op() {
    let mut s = base();
    let org = |p: &str| OrgRecord::RoleGranted {
        team: None,
        person: key(p),
        role: Role::OrgAdmin,
        repos: vec![],
    };
    let revoke = |p: &str| OrgRecord::RoleRevoked {
        team: None,
        person: key(p),
    };
    run(&mut s, org("ana")).unwrap();
    assert_eq!(run(&mut s, revoke("ana")), Err(OrgRefusal::LastOrgAdmin));
    assert_eq!(run(&mut s, revoke("bo")), Ok(Applied::Unchanged));
    run(&mut s, org("bo")).unwrap();
    assert_eq!(run(&mut s, revoke("ana")), Ok(Applied::Changed));
    let none = OrgRecord::RoleRevoked {
        team: Some(team("web")),
        person: key("bo"),
    };
    assert_eq!(run(&mut s, none), Ok(Applied::Unchanged));
}

#[test]
fn an_agent_name_is_unique_in_its_team_and_a_revoked_name_stays_taken() {
    let mut s = base();
    assert_eq!(
        run(&mut s, agent("payments", "w1", "t1")),
        Ok(Applied::Changed)
    );
    assert_eq!(
        run(&mut s, agent("payments", "w1", "t1")),
        Ok(Applied::Unchanged)
    );
    assert_eq!(
        run(&mut s, agent("payments", "w1", "t2")),
        Err(OrgRefusal::NameTaken)
    );
    assert_eq!(run(&mut s, agent("web", "w1", "t3")), Ok(Applied::Changed));
    let revoke = OrgRecord::AgentRevoked {
        team: team("payments"),
        agent: key("w1"),
    };
    assert_eq!(run(&mut s, revoke.clone()), Ok(Applied::Changed));
    assert_eq!(run(&mut s, revoke), Ok(Applied::Unchanged));
    assert_eq!(
        run(&mut s, agent("payments", "w1", "t4")),
        Err(OrgRefusal::NameTaken)
    );
    assert_eq!(
        run(&mut s, agent("payments", "w1", "t1")),
        Err(OrgRefusal::NameTaken)
    );
    assert_eq!(
        run(
            &mut s,
            OrgRecord::AgentRevoked {
                team: team("payments"),
                agent: key("ghost")
            }
        ),
        Err(OrgRefusal::UnknownAgent)
    );
}

#[test]
fn a_team_has_a_cap_on_agents() {
    let mut s = base();
    for i in 0..crate::org::MAX_AGENTS_PER_TEAM {
        run(&mut s, agent("web", &format!("a{i}"), &format!("t{i}"))).unwrap();
    }
    assert_eq!(
        run(&mut s, agent("web", "extra", "tx")),
        Err(OrgRefusal::TooMany)
    );
    assert_eq!(
        run(&mut s, agent("payments", "extra", "tx")),
        Ok(Applied::Changed)
    );
}

#[test]
fn a_revoked_agent_frees_its_place_but_not_its_name() {
    let mut s = base();
    for i in 0..crate::org::MAX_AGENTS_PER_TEAM {
        run(&mut s, agent("web", &format!("a{i}"), &format!("t{i}"))).unwrap();
    }
    run(
        &mut s,
        OrgRecord::AgentRevoked {
            team: team("web"),
            agent: key("a0"),
        },
    )
    .unwrap();
    assert_eq!(
        run(&mut s, agent("web", "a0", "t-again")),
        Err(OrgRefusal::NameTaken)
    );
    assert_eq!(
        run(&mut s, agent("web", "extra", "tx")),
        Ok(Applied::Changed)
    );
    assert_eq!(
        run(&mut s, agent("web", "extra2", "tx2")),
        Err(OrgRefusal::TooMany)
    );
}

#[test]
fn who_finds_each_kind_of_token_and_not_a_revoked_agent() {
    let mut s = base();
    run(&mut s, agent("payments", "w1", "t-w1")).unwrap();
    run(
        &mut s,
        OrgRecord::IngestAdded {
            team: team("web"),
            name: key("gh"),
            token_sha256: tok("t-gh"),
        },
    )
    .unwrap();
    assert_eq!(s.who("t-ana"), Some(Principal::Person(key("ana"))));
    assert_eq!(
        s.who("t-w1"),
        Some(Principal::Agent {
            team: team("payments"),
            agent: key("w1")
        })
    );
    assert_eq!(
        s.who("t-gh"),
        Some(Principal::Ingest {
            team: team("web"),
            name: key("gh")
        })
    );
    assert_eq!(s.who("nobody"), None);
    run(
        &mut s,
        OrgRecord::AgentRevoked {
            team: team("payments"),
            agent: key("w1"),
        },
    )
    .unwrap();
    assert_eq!(s.who("t-w1"), None);
}

#[test]
fn a_source_has_one_owner() {
    let mut s = base();
    let add = |t: &str| OrgRecord::SourceAdded {
        team: team(t),
        source: "github:GetFelix/felix".into(),
    };
    assert_eq!(run(&mut s, add("payments")), Ok(Applied::Changed));
    assert_eq!(run(&mut s, add("payments")), Ok(Applied::Unchanged));
    assert_eq!(run(&mut s, add("web")), Err(OrgRefusal::SourceOwned));
    assert_eq!(
        s.source_owner("github:GetFelix/felix"),
        Some(&team("payments"))
    );
    let bad = OrgRecord::SourceAdded {
        team: team("web"),
        source: "x\ny".into(),
    };
    assert_eq!(run(&mut s, bad), Err(OrgRefusal::BadLabel));
}

#[test]
fn a_grant_needs_two_teams_and_the_receivers_sources() {
    let mut s = base();
    let grant = |team_: &str, from: &str, repos: Vec<String>| OrgRecord::GrantAdded {
        team: team(team_),
        from_team: team(from),
        agents: vec![],
        repos,
        max_pending: Some(4),
    };
    assert_eq!(
        run(&mut s, grant("payments", "payments", vec![])),
        Err(OrgRefusal::OwnGrant)
    );
    assert_eq!(
        run(&mut s, grant("payments", "nope", vec![])),
        Err(OrgRefusal::UnknownTeam)
    );
    assert_eq!(
        run(&mut s, grant("payments", "web", vec!["github:x/y".into()])),
        Err(OrgRefusal::UnknownSource)
    );
    assert_eq!(
        run(&mut s, grant("payments", "web", vec![])),
        Ok(Applied::Changed)
    );
    assert_eq!(
        run(&mut s, grant("payments", "web", vec![])),
        Ok(Applied::Unchanged)
    );
    assert_eq!(s.grants().len(), 1);
}

#[test]
fn replay_skips_a_refused_record_and_matches_the_live_state() {
    let records = [
        create("web"),
        person("ana", "t-ana"),
        person("ana", "forged"),
        grant_role("web", "ana", Role::Admin),
        grant_role("web", "ghost", Role::Admin),
    ]
    .map(log);
    let state = fold(&records);
    assert_eq!(state.person(&key("ana")).unwrap().token, tok("t-ana"));
    assert!(state.person(&key("ghost")).is_none());
    assert_eq!(state.role_in(&team("web"), &key("ana")), Some(Role::Admin));
    assert_eq!(
        state.teams_of(&key("ana")),
        vec![(team("web"), Role::Admin)]
    );
}

#[test]
fn an_admin_cannot_be_narrowed_to_some_repos() {
    let mut s = base();
    run(
        &mut s,
        OrgRecord::SourceAdded {
            team: team("payments"),
            source: "github:a/c".into(),
        },
    )
    .unwrap();
    let narrow = OrgRecord::RoleGranted {
        team: Some(team("payments")),
        person: key("ana"),
        role: Role::Admin,
        repos: vec!["github:a/c".into()],
    };
    assert_eq!(run(&mut s, narrow), Err(OrgRefusal::RoleScope));
}

#[test]
fn a_team_repeated_with_other_fields_is_not_the_same_team() {
    let mut s = base();
    let again = OrgRecord::TeamCreated {
        team: team("web"),
        display_name: None,
        adopted: true,
    };
    assert_eq!(run(&mut s, again), Err(OrgRefusal::NameTaken));
}

fn at(v: u32, r: OrgRecord) -> OrgLogged {
    OrgLogged {
        v,
        ..log(r)
    }
}
fn source(t: &str, s: &str) -> OrgRecord {
    OrgRecord::SourceAdded {
        team: team(t),
        source: s.into(),
    }
}
fn connection(t: &str, name: &str, group: &str, sources: &[&str]) -> OrgRecord {
    OrgRecord::ConnectionSet {
        team: team(t),
        name: key(name),
        vendor: "github".into(),
        group: key(group),
        pool: key("gh-app"),
        sources: sources.iter().map(|s| s.to_string()).collect(),
    }
}
fn raised() -> OrgState {
    let mut s = OrgState::default();
    s.apply(&log(create("a"))).unwrap();
    s.apply(&log(create("b"))).unwrap();
    s.apply(&log(source("a", "github:o/one"))).unwrap();
    s.apply(&log(source("b", "github:o/two"))).unwrap();
    s.apply(&at(2, OrgRecord::FormatRaised { to: 2 })).unwrap();
    s
}

#[test]
fn format_2_records_wait_for_the_raise() {
    let mut s = OrgState::default();
    s.apply(&log(create("a"))).unwrap();
    s.apply(&log(source("a", "github:o/one"))).unwrap();
    let conn = connection("a", "gh", "g1", &["github:o/one"]);
    assert_eq!(s.apply(&log(conn.clone())), Err(OrgRefusal::Format));
    // A raise stamped below what it raises to would not stop an older reader.
    assert_eq!(
        s.apply(&at(1, OrgRecord::FormatRaised { to: 2 })),
        Err(OrgRefusal::Format)
    );
    s.apply(&at(2, OrgRecord::FormatRaised { to: 2 })).unwrap();
    assert_eq!(s.format(), 2);
    assert_eq!(
        s.apply(&at(2, OrgRecord::FormatRaised { to: 2 })),
        Err(OrgRefusal::Format),
        "the format never stays or goes down"
    );
    assert_eq!(s.apply(&log(conn)), Ok(Applied::Changed));
}

#[test]
fn an_older_reader_stops_at_the_raise() {
    let raise = OrgLogged::new(1, key("ana"), OrgRecord::FormatRaised { to: 3 });
    assert_eq!(raise.v, 3);
    let payload = crate::codec::Codec::encode(&raise);
    assert!(matches!(
        crate::org::read(&payload),
        Err(crate::org::OrgReadError::Newer { v: 3 })
    ));
}

#[test]
fn a_connection_serves_only_its_own_teams_sources_once() {
    let mut s = raised();
    assert_eq!(
        s.check(&log(connection("a", "gh", "g1", &["github:o/two"]))),
        Err(OrgRefusal::UnknownSource),
        "another team's source"
    );
    assert_eq!(
        s.check(&log(connection("a", "gh", "g1", &["github:o/one", "github:o/one"]))),
        Err(OrgRefusal::UnknownSource)
    );
    s.apply(&log(connection("a", "gh", "g1", &["github:o/one"]))).unwrap();
    assert_eq!(
        s.check(&log(connection("a", "gh2", "g2", &["github:o/one"]))),
        Err(OrgRefusal::SourceServed)
    );
    assert_eq!(
        s.check(&log(connection("a", "gh2", "g1", &[]))),
        Err(OrgRefusal::NameTaken),
        "one connection per group"
    );
    assert_eq!(
        s.apply(&log(connection("a", "gh", "g1", &["github:o/one"]))),
        Ok(Applied::Unchanged)
    );
    s.apply(&log(connection("b", "gh", "g1", &["github:o/two"]))).unwrap();
    let (name, c) = s.connection_of_group(&team("a"), &key("g1")).unwrap();
    assert_eq!((name.as_str(), c.sources.len()), ("gh", 1));
    assert_eq!(s.pool_sources(&key("gh-app")), 2);
}

#[test]
fn a_budget_keeps_back_no_more_than_it_has() {
    let mut s = raised();
    let budget = |per_hour, kept_back, wake_cap| OrgRecord::BudgetSet {
        pool: key("gh-app"),
        per_hour,
        kept_back,
        wake_cap,
    };
    assert_eq!(s.check(&log(budget(0, 0, 0))), Err(OrgRefusal::BadBudget));
    assert_eq!(s.check(&log(budget(10, 11, 0))), Err(OrgRefusal::BadBudget));
    assert_eq!(s.check(&log(budget(10, 5, 6))), Err(OrgRefusal::BadBudget));
    s.apply(&log(budget(4500, 900, 300))).unwrap();
    assert_eq!(s.budget(&key("gh-app")).unwrap().kept_back, 900);
}
