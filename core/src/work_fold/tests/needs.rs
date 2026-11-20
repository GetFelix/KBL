//! What an item's work needs, its vocabulary, and lifting every block on one
//! resource (format 20).

use super::*;
use crate::work::Vocabulary;

fn vocab(needs: &[&str]) -> WorkRecord {
    WorkRecord::RulesSet {
        target: Target {
            source: "github".into(),
            repo: "GetFelix/felix".into(),
        },
        root: None,
        text: None,
        checks: None,
        vocabulary: Some(Vocabulary {
            types: vec![],
            areas: vec![],
            sizes: vec![],
            needs: needs.iter().map(|n| (*n).to_owned()).collect(),
        }),
    }
}

fn list(needs: &[&str]) -> Option<Vec<String>> {
    Some(needs.iter().map(|n| (*n).to_owned()).collect())
}

fn edited(item: &str, needs: Option<Vec<String>>) -> WorkRecord {
    WorkRecord::Edited {
        item: id(item),
        title: None,
        tier: None,
        labels: None,
        milestone: None,
        item_type: None,
        area: None,
        size: None,
        needs,
        depends_on: None,
        split: None,
        proposal: None,
    }
}

fn triaged(item: &str, needs: Option<Vec<String>>) -> WorkRecord {
    WorkRecord::Triaged {
        item: id(item),
        tier: 1,
        why: "fits".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![],
        related: vec![],
        split: vec![],
        item_type: None,
        area: None,
        size: None,
        needs,
    }
}

fn import(item: &str) -> WorkRecord {
    WorkRecord::Imported {
        item: id(item),
        source: "github".into(),
        upstream_id: format!("GetFelix/felix#{item}"),
        title: format!("bug {item}"),
        labels: vec![],
        milestone: None,
    }
}

fn at_20() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 20 });
    l.person(vocab(&["azure", "gpu", "network"]));
    l.tracker(import("r1"));
    l
}

fn needs_of(l: &Log, item: &str) -> Vec<String> {
    fold(&l.0).items[&id(item)].class.needs().to_vec()
}

fn by(l: &Log, item: &str) -> Option<ClassBy> {
    fold(&l.0).items[&id(item)]
        .class
        .needs
        .as_ref()
        .map(|n| n.by)
}

#[test]
fn needs_come_from_the_vocabulary_as_it_spells_them() {
    let mut l = at_20();
    l.person(edited("r1", list(&["GPU", "azure", "gpu"])));
    assert!(fold(&l.0).rejections.is_empty());
    assert_eq!(needs_of(&l, "r1"), ["gpu", "azure"]);
    l.person(edited("r1", list(&["tpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
    assert_eq!(
        needs_of(&l, "r1"),
        ["gpu", "azure"],
        "a refusal changes nothing"
    );
    let nine: Vec<&str> = vec!["azure"; 9];
    l.person(edited("r1", list(&nine)));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
}

#[test]
fn without_a_vocabulary_no_need_is_accepted() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 20 });
    l.tracker(imported("r1"));
    l.person(edited("r1", list(&["gpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
}

#[test]
fn a_persons_needs_outlast_triage_and_a_rule_and_triage_replaces_a_rule() {
    let mut l = at_20();
    l.tracker(edited("r1", list(&["network"])));
    assert_eq!(by(&l, "r1"), Some(ClassBy::Rule));
    l.person(triaged("r1", list(&["gpu"])));
    assert_eq!(
        (needs_of(&l, "r1"), by(&l, "r1")),
        (vec!["gpu".to_owned()], Some(ClassBy::Triage))
    );
    // A rule never replaces triage's call.
    l.tracker(edited("r1", list(&["azure"])));
    assert_eq!(needs_of(&l, "r1"), ["gpu"]);
    // A person's set stands against both.
    l.person(edited("r1", list(&["azure"])));
    l.person(triaged("r1", list(&["gpu"])));
    l.tracker(edited("r1", list(&["network"])));
    assert_eq!(
        (needs_of(&l, "r1"), by(&l, "r1")),
        (vec!["azure".to_owned()], Some(ClassBy::Person))
    );
    // Only a person clears it.
    l.person(triaged("r1", list(&[])));
    assert_eq!(needs_of(&l, "r1"), ["azure"]);
    l.person(edited("r1", list(&[])));
    assert!(needs_of(&l, "r1").is_empty());
}

#[test]
fn a_triage_answer_with_a_bad_need_is_refused_whole() {
    let mut l = at_20();
    l.person(triaged("r1", list(&["tpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
    assert!(fold(&l.0).items[&id("r1")].proposal.is_none());
}

#[test]
fn an_agent_cannot_set_needs_and_they_do_not_mix_with_upstream_facts() {
    let mut l = at_20();
    l.agent("w1", edited("r1", list(&["gpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    let mut mixed = edited("r1", list(&["gpu"]));
    if let WorkRecord::Edited { labels, .. } = &mut mixed {
        *labels = Some(vec!["x".into()]);
    }
    l.tracker(mixed);
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
    l.tracker(edited("r1", list(&["gpu"])));
    l.tracker(edited("r1", list(&["gpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::NoChange));
}

#[test]
fn a_root_s_needs_apply_to_the_work_that_runs_and_not_to_triage_or_review() {
    let mut l = at_20();
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
        triage_skipped: None,
        admitted_by: None,
    });
    l.person(edited("r1", list(&["gpu", "azure"])));
    l.person(proposed("r1-t", Some("r1"), "k", WorkKind::Triage));
    let s = fold(&l.0);
    let caps = |c: &[&str]| c.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(s.needs_of(&id("r1")), ["gpu", "azure"]);
    assert!(!s.can_take(&id("r1"), &[]));
    assert!(!s.can_take(&id("r1"), &caps(&["gpu"])));
    assert!(s.can_take(&id("r1"), &caps(&["AZURE", "gpu", "network"])));
    assert!(s.items.contains_key(&id("r1-t")));
    assert!(s.can_take(&id("r1-t"), &[]));
    assert_eq!(s.items[&id("r1-t")].work, WorkKind::Triage);
}

#[test]
fn needs_are_refused_below_format_20_and_the_vocabulary_too() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 19 });
    l.person(vocab(&["gpu"]));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    l.tracker(imported("r1"));
    l.person(edited("r1", list(&["gpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    l.person(triaged("r1", list(&["gpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    l.person(WorkRecord::ResourceLifted {
        resource: "azure".into(),
    });
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    // A vocabulary without needs is still a format 19 record.
    assert_eq!(vocab(&[]).format(), crate::work::FORMAT_V19);
    assert_eq!(vocab(&["gpu"]).format(), crate::work::FORMAT_V20);
}

fn block(item: &str, until: Option<u64>, resource: Option<&str>) -> WorkRecord {
    WorkRecord::BlockSet {
        item: id(item),
        until_ms: until,
        resource: resource.map(str::to_owned),
        reason: "no credit".into(),
    }
}

fn lift(resource: &str) -> WorkRecord {
    WorkRecord::ResourceLifted {
        resource: resource.into(),
    }
}

fn blocked_now(l: &Log, item: &str) -> bool {
    fold(&l.0).blocked_at(&id(item), 1)
}

#[test]
fn lifting_a_resource_ends_only_the_blocks_that_name_it() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 20 });
    for i in ["a1", "a2", "a3", "a4", "a5", "a6"] {
        accepted_root(&mut l, i);
    }
    let far = 10_000_000;
    l.person(block("a1", None, Some("azure-vms")));
    // Both a time and the resource: the resource is what lifting acts on.
    l.person(block("a2", Some(far), Some("azure-vms")));
    l.person(block("a3", None, Some("gpu-pool")));
    l.person(block("a4", Some(far), None));
    // a5 is blocked and closed; a6 is not blocked at all.
    l.person(block("a5", None, Some("azure-vms")));
    l.person(WorkRecord::Closed {
        item: id("a5"),
        why: crate::work::CloseWhy::Dismissed,
    });
    let before = fold(&l.0);
    l.person(lift("azure-vms"));
    let s = fold(&l.0);
    assert!(
        s.rejections.len() == before.rejections.len(),
        "{:?}",
        s.rejections
    );
    assert!(!blocked_now(&l, "a1") && !blocked_now(&l, "a2"));
    assert!(blocked_now(&l, "a3"), "another resource stays");
    assert!(blocked_now(&l, "a4"), "a time-only block stays");
    assert!(
        s.items[&id("a5")].block.is_some(),
        "a closed item is left as it was"
    );
    assert!(s.items[&id("a6")].block.is_none());
}

#[test]
fn lifting_a_resource_nothing_waits_on_is_refused_and_only_a_person_lifts() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 20 });
    accepted_root(&mut l, "a1");
    l.person(lift("azure-vms"));
    assert_eq!(last_rejection(&l), Some(Reject::NoChange));
    l.person(block("a1", None, Some("azure-vms")));
    l.tracker(lift("azure-vms"));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.agent("w1", lift("azure-vms"));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    assert!(blocked_now(&l, "a1"));
    l.person(lift(""));
    assert_eq!(last_rejection(&l), Some(Reject::BadBlock));
    l.person(lift("azure-vms"));
    assert!(!blocked_now(&l, "a1"));
    // The names are exact: a different case is another resource.
    l.person(block("a1", None, Some("azure-vms")));
    l.person(lift("Azure-VMs"));
    assert_eq!(last_rejection(&l), Some(Reject::NoChange));
    assert!(blocked_now(&l, "a1"));
}

#[test]
fn triage_s_empty_list_does_not_remove_a_rules_needs() {
    let mut l = at_20();
    l.tracker(edited("r1", list(&["azure"])));
    l.person(triaged("r1", list(&[])));
    assert_eq!(needs_of(&l, "r1"), ["azure"]);
    // Its own set it may drop.
    l.person(triaged("r1", list(&["gpu"])));
    l.person(triaged("r1", list(&[])));
    assert!(needs_of(&l, "r1").is_empty());
}

#[test]
fn needs_on_an_item_that_is_not_a_root_are_refused() {
    let mut l = at_20();
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
        triage_skipped: None,
        admitted_by: None,
    });
    l.person(proposed("r1-t", Some("r1"), "k", WorkKind::Triage));
    l.person(edited("r1-t", list(&["gpu"])));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
}
