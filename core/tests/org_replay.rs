//! A log written by this format folds to the same state on every build. The
//! golden file is frozen: when the fold changes, this log must still produce
//! these tables (`docs/upgrades.md`).

use kbl_core::names::Key;
use kbl_core::org::{Role, TeamName, read};
use kbl_core::org_fold::{Principal, fold};

fn key(s: &str) -> Key {
    Key::new(s).unwrap()
}
fn team(s: &str) -> TeamName {
    TeamName::new(s).unwrap()
}

#[test]
fn the_golden_log_folds_to_its_tables() {
    let text = include_str!("org_wire.jsonl");
    let log: Vec<_> = text.lines().map(|l| read(l.as_bytes()).unwrap()).collect();
    let s = fold(&log);

    assert_eq!(s.teams().count(), 2);
    assert!(s.teams().any(|(t, v)| t.as_str() == "payments" && v.adopted));
    assert_eq!(s.role_in(&team("payments"), &key("ana")), Some(Role::OrgAdmin));
    assert_eq!(s.role_in(&team("web"), &key("ana")), Some(Role::OrgAdmin));
    // bo was removed from payments by the last record.
    assert_eq!(s.role_in(&team("payments"), &key("bo")), None);
    assert_eq!(s.teams_of(&key("bo")), vec![]);
    assert_eq!(
        s.who("w1-token"),
        Some(Principal::Agent {
            team: team("payments"),
            agent: key("w1")
        })
    );
    assert_eq!(s.who("web-w1-token"), None, "revoked");
    assert_eq!(
        s.who("gh-token"),
        Some(Principal::Ingest {
            team: team("payments"),
            name: key("github")
        })
    );
    assert_eq!(s.source_owner("github:GetFelix/felix"), Some(&team("payments")));
    assert_eq!(s.grants().len(), 1);
}
