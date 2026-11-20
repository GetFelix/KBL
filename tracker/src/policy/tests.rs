use kbl_core::names::{ItemId, Key};
use kbl_core::work::{Actor, Target, Vocabulary, WorkLogged, WorkRecord};
use kbl_core::work_fold::State;

use super::*;

fn state(tier: u8, area: Option<&str>, size: Option<&str>, needs: &[&str]) -> (State, ItemId) {
    let id = ItemId::new("r1").unwrap();
    let list = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let records = [
        (
            Actor::Person,
            WorkRecord::FormatRaised {
                to: kbl_core::work::FORMAT,
            },
        ),
        (
            Actor::Person,
            WorkRecord::RulesSet {
                target: Target {
                    source: "github".into(),
                    repo: "GetFelix/felix".into(),
                },
                root: None,
                text: None,
                checks: None,
                vocabulary: Some(Vocabulary {
                    types: list(&["bug", "docs"]),
                    areas: list(&["docs", "storage", "ci"]),
                    sizes: list(&["S", "M", "L"]),
                    needs: list(&["safety-review", "gpu"]),
                }),
            },
        ),
        (
            Actor::Tracker,
            WorkRecord::Imported {
                item: id.clone(),
                source: "github".into(),
                upstream_id: "GetFelix/felix#1".into(),
                title: "t".into(),
                labels: vec![],
                milestone: None,
            },
        ),
        (
            Actor::Person,
            WorkRecord::Triaged {
                item: id.clone(),
                tier,
                why: "w".into(),
                duplicate_of: None,
                unclear: None,
                depends_on: vec![],
                related: vec![],
                split: vec![],
                item_type: Some("bug".into()),
                area: area.map(str::to_owned),
                size: size.map(str::to_owned),
                needs: Some(list(needs)),
            },
        ),
    ];
    let mut s = State::default();
    for (at, (actor, r)) in records.into_iter().enumerate() {
        let logged = WorkLogged::new(1000, Key::new("t").unwrap(), actor, r);
        s.try_apply(at as u64, &logged).expect("the fold takes it");
    }
    (s, id)
}

fn policy(rules: &str) -> TriagePolicy {
    TriagePolicy::parse(&format!(r#"{{"rules": {rules}}}"#)).unwrap()
}

const EXAMPLE: &str = r#"[
  {"name": "routine", "tiers": [2, 3], "deny_areas": ["storage"], "deny_needs": ["safety-review"]}
]"#;

#[test]
fn a_rule_matches_what_triage_recorded() {
    let p = policy(EXAMPLE);
    let hit = |tier, area, needs: &[&str]| {
        let (s, id) = state(tier, area, Some("S"), needs);
        p.matching(&s.items[&id]).map(str::to_owned)
    };
    assert_eq!(hit(2, Some("docs"), &[]).as_deref(), Some("routine"));
    assert_eq!(hit(3, None, &[]).as_deref(), Some("routine"));
    // A tier outside the list, a denied area and a denied need each keep it with a person.
    assert_eq!(hit(0, Some("docs"), &[]), None);
    assert_eq!(hit(2, Some("storage"), &[]), None);
    assert_eq!(hit(2, Some("docs"), &["safety-review"]), None);
    // Other needs are fine.
    assert_eq!(hit(2, Some("docs"), &["gpu"]).as_deref(), Some("routine"));
}

#[test]
fn an_allow_list_needs_the_field_to_be_recorded_and_listed() {
    let p =
        policy(r#"[{"name": "docs", "areas": ["docs"], "sizes": ["S", "M"], "types": ["bug"]}]"#);
    let hit = |area, size| {
        let (s, id) = state(2, area, size, &[]);
        p.matching(&s.items[&id]).is_some()
    };
    assert!(hit(Some("docs"), Some("S")));
    assert!(!hit(None, Some("S")), "no area recorded");
    assert!(!hit(Some("ci"), Some("S")));
    assert!(!hit(Some("docs"), Some("L")));
    assert!(!hit(Some("docs"), None));
}

#[test]
fn the_first_matching_rule_names_the_acceptance() {
    let p =
        policy(r#"[{"name": "small", "sizes": ["S"]}, {"name": "any", "tiers": [0, 1, 2, 3]}]"#);
    let (s, id) = state(1, None, Some("S"), &[]);
    assert_eq!(p.matching(&s.items[&id]), Some("small"));
    let (s, id) = state(1, None, Some("L"), &[]);
    assert_eq!(p.matching(&s.items[&id]), Some("any"));
}

#[test]
fn a_bad_policy_is_refused_at_start() {
    for (bad, why) in [
        (r#"{"rules": [{"name": ""}]}"#, "name"),
        (r#"{"rules": [{"name": "a"}]}"#, "no condition"),
        (r#"{"rules": [{"name": "a", "tiers": [4]}]}"#, "tier"),
        (
            r#"{"rules": [{"name": "a", "tiers": [1]}, {"name": "a", "tiers": [2]}]}"#,
            "twice",
        ),
        (
            r#"{"rules": [{"name": "a", "tiers": [1], "area": ["x"]}]}"#,
            "unknown field",
        ),
        (r#"{"rules": [{"name": "a", "areas": [""]}]}"#, "empty"),
        (r#"{"rule": []}"#, "unknown field"),
    ] {
        let err = TriagePolicy::parse(bad).unwrap_err().to_string();
        assert!(err.contains(why), "{why}: {err}");
    }
    assert!(TriagePolicy::parse(r#"{"rules": []}"#).unwrap().is_empty());
}
