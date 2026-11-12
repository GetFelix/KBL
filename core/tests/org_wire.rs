use kbl_core::codec::Codec;
use kbl_core::names::Key;
use kbl_core::org::*;

fn key(s: &str) -> Key {
    Key::new(s).unwrap()
}
fn team(s: &str) -> TeamName {
    TeamName::new(s).unwrap()
}

/// One record of every kind, as it is logged.
fn every_record() -> Vec<OrgLogged> {
    let at = |n: u64, r| OrgLogged::new(1_762_900_000_000 + n, key("ana"), r);
    vec![
        at(
            0,
            OrgRecord::TeamCreated {
                team: team("payments"),
                display_name: Some("Payments".into()),
                adopted: true,
            },
        ),
        at(
            1,
            OrgRecord::TeamCreated {
                team: team("web"),
                display_name: None,
                adopted: false,
            },
        ),
        at(
            2,
            OrgRecord::PersonAdded {
                person: key("ana"),
                token_sha256: TokenHash::of("ana-token"),
                subject: Some("auth0|ana".into()),
            },
        ),
        at(
            3,
            OrgRecord::PersonAdded {
                person: key("bo"),
                token_sha256: TokenHash::of("bo-token"),
                subject: None,
            },
        ),
        at(
            4,
            OrgRecord::RoleGranted {
                team: None,
                person: key("ana"),
                role: Role::OrgAdmin,
                repos: vec![],
            },
        ),
        at(
            5,
            OrgRecord::RoleGranted {
                team: Some(team("payments")),
                person: key("ana"),
                role: Role::Admin,
                repos: vec![],
            },
        ),
        at(
            6,
            OrgRecord::SourceAdded {
                team: team("payments"),
                source: "github:GetFelix/felix".into(),
            },
        ),
        at(
            7,
            OrgRecord::RoleGranted {
                team: Some(team("payments")),
                person: key("bo"),
                role: Role::Triager,
                repos: vec!["github:GetFelix/felix".into()],
            },
        ),
        at(
            8,
            OrgRecord::AgentAdded {
                team: team("payments"),
                agent: key("w1"),
                token_sha256: TokenHash::of("w1-token"),
            },
        ),
        at(
            9,
            OrgRecord::AgentAdded {
                team: team("web"),
                agent: key("w1"),
                token_sha256: TokenHash::of("web-w1-token"),
            },
        ),
        at(
            10,
            OrgRecord::AgentRevoked {
                team: team("web"),
                agent: key("w1"),
            },
        ),
        at(
            11,
            OrgRecord::IngestAdded {
                team: team("payments"),
                name: key("github"),
                token_sha256: TokenHash::of("gh-token"),
            },
        ),
        at(
            12,
            OrgRecord::GrantAdded {
                team: team("payments"),
                from_team: team("web"),
                agents: vec![key("w1")],
                repos: vec!["github:GetFelix/felix".into()],
                max_pending: Some(4),
            },
        ),
        at(
            13,
            OrgRecord::RoleRevoked {
                team: Some(team("payments")),
                person: key("bo"),
            },
        ),
    ]
}

/// To change the format on purpose, regenerate with `WRITE_WIRE=1 cargo test -p kbl-core --test org_wire`.
#[test]
fn wire_form_matches_the_golden_file() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/org_wire.jsonl");
    let now: String = every_record()
        .iter()
        .map(|r| String::from_utf8(r.encode()).unwrap() + "\n")
        .collect();
    if std::env::var_os("WRITE_WIRE").is_some() {
        std::fs::write(path, &now).unwrap();
    }
    assert_eq!(now, std::fs::read_to_string(path).unwrap());
}

#[test]
fn every_kind_round_trips_and_is_covered() {
    let all = every_record();
    for r in &all {
        assert_eq!(&read(&r.encode()).unwrap(), r);
        let line = String::from_utf8(r.encode()).unwrap();
        assert!(!line.contains(r#""v":"#), "format 1 is written without v");
        assert!(line.contains(&format!(r#""kind":"{}""#, r.record.kind())));
    }
    let mut kinds: Vec<_> = all.iter().map(|r| r.record.kind()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(kinds.len(), 9);
}

#[test]
fn a_log_holds_hashes_never_tokens() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/org_wire.jsonl"))
        .unwrap();
    for token in ["ana-token", "bo-token", "w1-token", "gh-token"] {
        assert!(!text.contains(token));
        assert!(text.contains(TokenHash::of(token).as_str()));
    }
}

#[test]
fn a_newer_format_is_refused_not_skipped() {
    let newer = br#"{"v":2,"at_ms":1,"by":"ana","kind":"team_created","team":"web"}"#;
    assert!(matches!(read(newer), Err(OrgReadError::Newer { v: 2 })));
    let unknown = br#"{"at_ms":1,"by":"ana","kind":"mystery"}"#;
    assert!(matches!(read(unknown), Err(OrgReadError::Malformed(_))));
    let bad_hash = br#"{"at_ms":1,"by":"ana","kind":"agent_added","team":"web","agent":"w1","token_sha256":"abc"}"#;
    assert!(matches!(read(bad_hash), Err(OrgReadError::Malformed(_))));
    let v1 = br#"{"v":1,"at_ms":1,"by":"ana","kind":"team_created","team":"web"}"#;
    assert!(read(v1).is_ok());
}
