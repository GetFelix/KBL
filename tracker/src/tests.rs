use super::*;

#[test]
fn plain_http_listens_only_on_loopback_without_a_tls_proxy() {
    let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
    for ok in [
        "127.0.0.1:8790",
        "127.0.0.2:8790",
        "[::1]:8790",
        "[::ffff:127.0.0.1]:8790",
    ] {
        check_listen(addr(ok), false).unwrap();
    }
    for open in [
        "0.0.0.0:8790",
        "10.0.0.5:8790",
        "169.254.1.1:8790",
        "[::]:8790",
        "[fe80::1]:8790",
    ] {
        assert!(check_listen(addr(open), false).is_err(), "{open}");
        check_listen(addr(open), true).unwrap();
    }
}

#[test]
fn a_token_file_agent_that_agent_add_minted_and_nobody_revoked_stops_the_start() {
    use kbl_core::org::{OrgLogged, TokenHash};
    let team = TeamName::new("default").unwrap();
    let ops = || Key::new("ops").unwrap();
    let mut log = vec![
        OrgLogged::new(
            0,
            ops(),
            OrgRecord::TeamCreated {
                team: team.clone(),
                display_name: None,
                adopted: true,
            },
        ),
        OrgLogged::new(
            0,
            ops(),
            OrgRecord::AgentAdded {
                team: team.clone(),
                agent: Key::new("w1").unwrap(),
                token_sha256: TokenHash::of("tok-minted"),
            },
        ),
    ];
    let tokens = Tokens::parse(r#"{"agents":{"tok-w1":"w1","tok-w2":"w2"}}"#).unwrap();
    let err = file_agents_not_minted(&kbl_core::org_fold::fold(&log), &team, &tokens)
        .unwrap_err()
        .to_string();
    assert!(err.contains("agent w1"), "{err}");
    log.push(OrgLogged::new(
        0,
        ops(),
        OrgRecord::AgentRevoked {
            team: team.clone(),
            agent: Key::new("w1").unwrap(),
        },
    ));
    file_agents_not_minted(&kbl_core::org_fold::fold(&log), &team, &tokens).unwrap();
}
