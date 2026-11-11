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
