use super::*;

#[test]
fn the_principal_is_the_hex_sha256_of_issuer_and_subject() {
    // sha256("abc"), checked against the digest of the same input.
    assert_eq!(
        TokenHash::of("abc").as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let id = principal("http://idp", "tracker");
    assert_eq!(id, TokenHash::of("http://idp|tracker").as_str());
    assert_eq!(id.len(), 64);
}

#[test]
fn the_tracker_manages_each_namespace_it_is_given() {
    let policies = tracker_policies("agents", &["kbl", "default"]);
    assert_eq!(
        policies[1],
        json!({"subject": "role:kbl-tracker", "object": "namespace:agents/default", "action": "ns.manage"})
    );
    assert_eq!(policies.len(), 2);
}

#[test]
fn a_team_gets_manage_for_the_tracker_and_a_viewer_role_on_its_log() {
    let policies = team_policies("agents", "alpha");
    assert_eq!(policies.len(), 2);
    assert_eq!(policies[0]["object"], "namespace:agents/alpha");
    assert_eq!(policies[1]["subject"], "role:kbl-alpha-viewer");
    assert_eq!(policies[1]["object"], "stream:agents/alpha/work.events");
    assert_eq!(policies[1]["action"], "stream.subscribe");
}

#[test]
fn the_org_batch_has_the_log_and_the_cache() {
    let batch = org_batch();
    assert_eq!(batch["streams"][0]["stream"], "org.events");
    assert_eq!(batch["streams"][0]["durable"], true);
    assert_eq!(batch["caches"][0]["cache"], "org.tracker");
}

#[test]
fn the_organization_namespace_and_bad_names_are_not_team_names() {
    assert!(team_name("kbl").is_err());
    assert!(team_name("a b").is_err());
    assert!(team_name("").is_err());
    assert!(team_name("alpha").is_ok());
}
