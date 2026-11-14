use std::collections::BTreeMap;

use super::*;
use crate::content::Content;
use crate::names::Key;
use crate::work::{Actor, CloseWhy, WorkLogged, WorkRecord};
use crate::work_fold::fold;

fn id(s: &str) -> ItemId {
    ItemId::new(s).unwrap()
}

/// A log and the inline text of each context record in it.
#[derive(Default)]
struct Log {
    records: Vec<(u64, WorkLogged)>,
    texts: BTreeMap<u64, String>,
}

impl Log {
    fn push(&mut self, by: &str, actor: Actor, record: WorkRecord) {
        let offset = self.records.len() as u64;
        if let WorkRecord::Context {
            text: Content::Text(t),
            ..
        } = &record
        {
            self.texts.insert(offset, t.clone());
        }
        let logged = WorkLogged::new(offset, Key::new(by).unwrap(), actor, record);
        self.records.push((offset, logged));
    }
    fn person(&mut self, record: WorkRecord) {
        self.push("gabriel", Actor::Person, record);
    }
    fn root(&mut self, item: &str) {
        self.push(
            "tracker",
            Actor::Tracker,
            WorkRecord::Imported {
                item: id(item),
                source: "github".into(),
                upstream_id: format!("GetFelix/felix#{item}"),
                title: item.into(),
            },
        );
        self.person(WorkRecord::Accepted {
            item: id(item),
            tier: Some(1),
        });
    }
    fn files(&mut self, item: &str, text: &str) {
        self.person(WorkRecord::Context {
            item: id(item),
            entry: Key::new(FILES_ENTRY).unwrap(),
            text: Content::Text(text.into()),
        });
    }
    fn overlaps(&self) -> Vec<Overlap> {
        let state = fold(&self.records);
        overlaps(&state, |o| self.texts.get(&o).map(String::as_str))
    }
}

fn overlap(first: &str, then: &str, files: &[&str]) -> Overlap {
    Overlap {
        first: id(first),
        then: id(then),
        files: files.iter().map(|f| f.to_string()).collect(),
    }
}

#[test]
fn parse_files_trims_skips_comments_and_dedups() {
    let text = "  ./core/src/work.rs \n\n# the fold\ncore/src/work.rs\ndocs/\n";
    assert_eq!(parse_files(text), ["core/src/work.rs", "docs/"]);
}

#[test]
fn two_open_items_naming_one_file_overlap() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.files("r1", "core/src/work.rs\nREADME.md");
    l.files("r2", "core/src/work.rs\ndocs/a.md");
    assert_eq!(
        l.overlaps(),
        [overlap("r1", "r2", &["core/src/work.rs"])],
        "the item that declared its files first goes first"
    );
}

#[test]
fn a_directory_covers_the_files_under_it() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.root("r3");
    l.files("r1", "docs/");
    l.files("r2", "docs/a.md\ndocs/b.md");
    l.files("r3", "docsite/x.md");
    assert_eq!(
        l.overlaps(),
        [overlap("r1", "r2", &["docs/a.md", "docs/b.md"])]
    );
}

#[test]
fn held_work_goes_first() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.files("r1", "a.rs");
    l.files("r2", "a.rs");
    l.push(
        "w1",
        Actor::Agent,
        WorkRecord::Claimed {
            item: id("r2"),
            attempt: 1,
            deadline_ms: 90_000,
        },
    );
    assert_eq!(l.overlaps(), [overlap("r2", "r1", &["a.rs"])]);
}

#[test]
fn linked_closed_and_undeclared_items_are_left_out() {
    let mut l = Log::default();
    for r in ["r1", "r2", "r3", "r4"] {
        l.root(r);
    }
    l.files("r1", "a.rs");
    l.files("r2", "a.rs");
    l.files("r3", "a.rs");
    l.person(WorkRecord::Linked {
        item: id("r2"),
        to: id("r1"),
    });
    l.person(WorkRecord::Closed {
        item: id("r3"),
        why: CloseWhy::Dismissed,
    });
    assert_eq!(l.overlaps(), []);
}

#[test]
fn the_latest_files_entry_counts() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.files("r1", "a.rs");
    l.files("r2", "a.rs");
    l.files("r2", "b.rs");
    assert_eq!(l.overlaps(), []);
}
