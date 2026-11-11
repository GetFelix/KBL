use super::*;

#[test]
fn a_token_maps_to_its_agent_or_person() {
    let tokens = Tokens::parse(r#"{"agents":{"t-w1":"w1"},"people":{"t-gab":"gabriel"}}"#).unwrap();
    assert!(matches!(tokens.who("t-w1"), Some(Caller::Agent(a)) if a.as_str() == "w1"));
    assert!(matches!(tokens.who("t-gab"), Some(Caller::Person(p)) if p.as_str() == "gabriel"));
    assert!(tokens.who("other").is_none());
}

#[test]
fn a_bad_agent_name_is_refused_at_load() {
    assert!(Tokens::parse(r#"{"agents":{"t":"has-dash"}}"#).is_err());
}

#[test]
fn a_token_listed_twice_or_for_both_kinds_is_refused() {
    assert!(Tokens::parse(r#"{"agents":{"t":"w1","t":"w2"}}"#).is_err());
    assert!(Tokens::parse(r#"{"agents":{"t":"w1"},"people":{"t":"gab"}}"#).is_err());
    assert!(Tokens::parse(r#"{"agents":{"":"w1"}}"#).is_err());
}

#[cfg(unix)]
#[test]
fn the_token_file_must_be_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("tokens-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tokens.json");
    std::fs::write(&path, r#"{"agents":{"t":"w1"}}"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Tokens::load(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Tokens::load(&path).is_ok());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert!(Tokens::load(&path).is_ok());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(Tokens::load(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tokens_of_other_lengths_or_with_a_shared_prefix_do_not_match() {
    let tokens = Tokens::parse(r#"{"agents":{"abcdef":"w1"}}"#).unwrap();
    assert!(tokens.who("abcde").is_none());
    assert!(tokens.who("abcdefg").is_none());
    assert!(tokens.who("abcdeg").is_none());
    assert!(tokens.who("abcdef").is_some());
}

#[test]
fn grants_name_agents_and_owner_repo_pairs() {
    let ok = Tokens::parse(r#"{"agents":{"t":"w1"},"grants":{"w1":["GetFelix/felix-gateway"]}}"#)
        .unwrap();
    assert_eq!(ok.grants()["w1"], ["GetFelix/felix-gateway"]);
    assert!(Tokens::parse(r#"{"agents":{"t":"w1"},"grants":{"w2":["a/b"]}}"#).is_err());
    assert!(Tokens::parse(r#"{"agents":{"t":"w1"},"grants":{"w1":["*"]}}"#).is_err());
    assert!(
        Tokens::parse(r#"{"agents":{"t":"w1"}}"#)
            .unwrap()
            .grants()
            .is_empty()
    );
}
