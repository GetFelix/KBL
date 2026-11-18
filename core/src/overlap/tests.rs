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
        self.root_in(item, "GetFelix/felix");
    }
    fn root_in(&mut self, item: &str, repo: &str) {
        self.push(
            "tracker",
            Actor::Tracker,
            WorkRecord::Imported {
                item: id(item),
                source: "github".into(),
                upstream_id: format!("{repo}#{item}"),
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
    fn claim(&mut self, item: &str) {
        self.push(
            "w1",
            Actor::Agent,
            WorkRecord::Claimed {
                item: id(item),
                attempt: 1,
                deadline_ms: 90_000,
                direct: false,
            },
        );
    }
    fn done(&mut self, item: &str) {
        self.push(
            "w1",
            Actor::Agent,
            WorkRecord::Done {
                item: id(item),
                attempt: 1,
                cost_micros: 0,
                usage: Vec::new(),
                floor_micros: 0,
            },
        );
    }
    fn link(&mut self, item: &str, to: &str) {
        self.person(WorkRecord::Linked {
            item: id(item),
            to: id(to),
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
fn parse_files_normalises_paths() {
    let text =
        "/core//src/./work.rs\ncore\\src\\fold.rs\ndocs/old/../a.md\nsrc/x/../\n../outside.rs\n";
    assert_eq!(
        parse_files(text),
        ["core/src/fold.rs", "core/src/work.rs", "docs/a.md", "src/"]
    );
    for whole in ["./", "/", "."] {
        assert_eq!(parse_files(whole), [WHOLE_REPO], "{whole}");
    }
    for skipped in ["a/..", "a/b/../..", "//", "\\", "..", "a/../.."] {
        assert_eq!(parse_files(skipped), Vec::<String>::new(), "{skipped}");
    }
}

#[test]
fn a_last_segment_of_dot_or_dotdot_is_a_directory() {
    assert_eq!(parse_files("core/src/."), ["core/src/"]);
    assert_eq!(parse_files("a/b/.."), ["a/"]);
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.root("r3");
    l.files("r1", "core/src/.");
    l.files("r2", "core/src/work.rs");
    l.files("r3", "a/b/..\n");
    l.files("r2", "core/src/work.rs\na/x.rs");
    assert_eq!(
        l.overlaps(),
        [
            overlap("r1", "r2", &["core/src/work.rs"]),
            overlap("r3", "r2", &["a/x.rs"]),
        ]
    );
}

#[test]
fn held_goes_before_in_review() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.files("r1", "a.rs");
    l.files("r2", "a.rs");
    l.claim("r1");
    l.done("r1");
    l.claim("r2");
    assert_eq!(l.overlaps(), [overlap("r2", "r1", &["a.rs"])]);
}

#[test]
fn in_review_goes_before_ready() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.files("r1", "a.rs");
    l.files("r2", "a.rs");
    l.claim("r2");
    l.done("r2");
    assert_eq!(l.overlaps(), [overlap("r2", "r1", &["a.rs"])]);
}

#[test]
fn items_with_no_target_are_compared_with_each_other() {
    let mut l = Log::default();
    l.root_in("r1", "");
    l.root_in("r2", "");
    l.root("r3");
    l.files("r1", "a.rs");
    l.files("r2", "a.rs");
    l.files("r3", "a.rs");
    assert_eq!(l.overlaps(), [overlap("r1", "r2", &["a.rs"])]);
}

#[test]
fn the_whole_repo_overlaps_every_path_in_it() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.files("r1", "./");
    l.files("r2", "a.rs\ndocs/");
    assert_eq!(l.overlaps(), [overlap("r1", "r2", &["a.rs", "docs/"])]);
}

#[test]
fn items_in_different_repos_do_not_overlap() {
    let mut l = Log::default();
    l.root_in("r1", "GetFelix/felix");
    l.root_in("r2", "GetFelix/KBL");
    l.files("r1", "README.md");
    l.files("r2", "README.md");
    assert_eq!(l.overlaps(), []);
}

#[test]
fn a_pair_joined_by_a_chain_of_links_is_left_out() {
    let mut l = Log::default();
    for r in ["r1", "r2", "r3"] {
        l.root(r);
        l.files(r, "a.rs");
    }
    l.link("r2", "r1");
    l.link("r3", "r2");
    // r3 reaches r1 through r2, so "r1 waits on r3" would be a cycle.
    assert_eq!(l.overlaps(), []);
}

#[test]
fn a_held_item_with_an_open_question_still_goes_first() {
    let mut l = Log::default();
    l.root("r1");
    l.root("r2");
    l.files("r1", "a.rs");
    l.files("r2", "a.rs");
    l.claim("r2");
    l.push(
        "w1",
        Actor::Agent,
        WorkRecord::Question {
            item: id("r2"),
            attempt: 1,
            text: Content::Text("which way?".into()),
            options: vec![],
            default: None,
            cost_micros: 0,
            usage: Vec::new(),
            floor_micros: 0,
        },
    );
    assert_eq!(l.overlaps(), [overlap("r2", "r1", &["a.rs"])]);
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
    l.claim("r2");
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
