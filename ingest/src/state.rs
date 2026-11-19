//! What ingest remembers between polls. It is kept in the tracker, so a
//! restart (or a second worker started by mistake) resumes without importing
//! again. The log stays the record of what happened; this is only bookkeeping.

use std::collections::{BTreeMap, BTreeSet};

use kbl_core::names::{ItemId, Key};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Rule;

use crate::source::{Closed, Cursor};

/// One imported issue, by upstream id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tracked {
    pub(crate) item: ItemId,
    /// The rule that admitted it.
    pub(crate) rule: String,
    /// Title, body and labels as last seen, hashed, to tell an edit from a
    /// change that touched none of them.
    pub(crate) fingerprint: String,
    pub(crate) title: String,
    /// The labels and milestone the tracker holds for the item, as last
    /// written. A difference from upstream's makes reconcile write an edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) closed: Option<Closed>,
    /// Whether it still matches its rule.
    pub(crate) in_scope: bool,
    /// Comments already recorded, by id, with the update time they had.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) comments: BTreeMap<u64, String>,
    /// Issues it waits on that are not imported yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) pending_links: BTreeSet<String>,
    /// Issues it waits on that already have an edge.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) linked: BTreeSet<String>,
    /// Issues it waits on whose edge the tracker refused as someone else's
    /// (`already_linked`), so the link is not retried on every touch. Kept
    /// until upstream drops the dependency or the next reconcile.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) not_ours: BTreeSet<String>,
    /// The pull request already made a draft after a drop.
    #[serde(default)]
    pub(crate) written_back: bool,
    /// Failed write-back attempts; it is given up on after a few.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) write_back_tries: u8,
    /// Upstream notices already made, as `linked:<pr>` or `merged:<pr>`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) announced: BTreeSet<String>,
    /// Failed notice attempts for the one in progress.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) announce_tries: u8,
    /// Set on the old id of a transferred issue, which is not fetched again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) moved_to: Option<String>,
    /// An issue ingest opened for a prerequisite. No intake rule scopes it:
    /// a person already accepted the work.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) prerequisite: bool,
    /// The last failed check reported as `check_failed`: request, check and
    /// head commit, so a re-poll of the same red does not fetch its log again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) check_reported: Option<(String, String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    /// Where each repo's polling resumes, so a repo that cannot be read
    /// stalls only itself.
    #[serde(default)]
    pub(crate) cursors: BTreeMap<String, Cursor>,
    /// The single cursor older versions kept for all repos. Read once: the
    /// first poll gives it to every repo and drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cursor: Option<Cursor>,
    /// A digest of each rule's selector, by rule name, as of the last poll.
    /// A rule that is new or whose digest changed makes its repo read in full
    /// once, since the cursor only covers what the old rule asked for. State
    /// saved before this existed has none, which reads as unchanged.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) selectors: BTreeMap<String, String>,
    /// A digest of each rule's `label_needs`, by rule name, for rules that
    /// have one. A new or changed map re-maps the rule's existing issues.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) need_maps: BTreeMap<String, String>,
    /// Repos last reported as unreadable, with the status, so the report is
    /// written when it changes and not on every poll.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) stalled: BTreeMap<String, u16>,
    /// Upstream id (`owner/repo#n`) to the issue. A transferred issue has an
    /// entry under each id.
    pub(crate) items: BTreeMap<String, Tracked>,
    /// Matches left out because their rule was at its cap, oldest first.
    pub(crate) waiting: BTreeSet<String>,
    /// The last issue the spread reconcile re-read, so the next poll goes on
    /// after it. Missing starts at the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reconcile_after: Option<String>,
    /// Merged roots a link waits on, by upstream id, until one ships.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) awaiting: BTreeMap<String, Awaiting>,
    /// Prerequisite item to the issue ingest opened for it, until the
    /// tracker ties them, so a lost write never opens a second issue.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) opened: BTreeMap<String, String>,
    /// Failed checks ingest re-ran as a known flake, by upstream id (or
    /// `source:id` for a foreign root), until the re-run settles.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) reruns: BTreeMap<String, Rerun>,
}

/// One flake re-run in progress, or already spent for this commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Rerun {
    pub(crate) pr: String,
    /// The head commit that failed; a push starts over.
    pub(crate) head: String,
    pub(crate) check: String,
    /// The failed check's link, which a re-run replaces.
    pub(crate) url: String,
    pub(crate) flake: Key,
    /// The one re-run for this commit is used up: a later red goes the
    /// normal way.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) spent: bool,
    /// Polls that still showed the old failure after the re-run started.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) waits: u8,
}

/// What a merged root's release search has learned, so no request is repeated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Awaiting {
    /// The pull request this was read from; another one starts over.
    pub(crate) pr: String,
    /// The merge commit and when it was made. None when GitHub gave neither,
    /// or the merge was not one that counts: nothing can ship it.
    pub(crate) merge: Option<(String, u64)>,
    /// Tags already compared that do not contain the merge.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) misses: BTreeSet<String>,
    /// Polls left to skip this root after a lookup that cannot succeed soon.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub(crate) skip: u32,
}

impl State {
    /// Drops the cursor of every repo with a rule that is new or selects
    /// something else than it did, so the next poll reads that repo in full.
    /// Unchanged rules keep their cursors.
    pub(crate) fn reset_changed(&mut self, rules: &[Rule]) {
        let upgrading = self.selectors.is_empty();
        let now: BTreeMap<String, String> = rules
            .iter()
            .map(|r| (r.name.clone(), selector_digest(r)))
            .collect();
        if !upgrading {
            for r in rules {
                if self.selectors.get(&r.name) != now.get(&r.name) {
                    self.cursors.remove(&r.repo);
                }
            }
        }
        self.selectors = now;
        self.remap_needs(rules, upgrading);
    }

    /// A rule whose `label_needs` is new or changed reads its repo in full
    /// and forgets the labels it last wrote for its items, so the next
    /// reconcile of each maps them again. Existing issues would otherwise be
    /// mapped only when their labels next change. Rewriting the same needs is
    /// a no-op in the tracker, so a repeat or a crash retry is harmless.
    fn remap_needs(&mut self, rules: &[Rule], upgrading: bool) {
        let now: BTreeMap<String, String> = rules
            .iter()
            .filter(|r| !r.label_needs.is_empty())
            .map(|r| (r.name.clone(), needs_digest(r)))
            .collect();
        if !upgrading {
            for r in rules {
                if self.need_maps.get(&r.name) == now.get(&r.name) || r.label_needs.is_empty() {
                    continue;
                }
                self.cursors.remove(&r.repo);
                for t in self.items.values_mut().filter(|t| t.rule == r.name) {
                    t.labels.clear();
                }
            }
        }
        self.need_maps = now;
    }

    /// Moves the old shared cursor into one cursor per repo in `repos` that has none.
    pub(crate) fn migrate(&mut self, repos: &[String]) {
        if let Some(old) = self.cursor.take() {
            for repo in repos {
                self.cursors
                    .entry(repo.clone())
                    .or_insert_with(|| old.clone());
            }
        }
    }
}

/// What a rule selects, hashed: the fields that decide which issues it
/// admits, not how it treats them.
fn selector_digest(r: &Rule) -> String {
    let sel = serde_json::json!([
        r.repo.to_ascii_lowercase(),
        r.labels_all,
        r.labels_any,
        r.labels_excluded,
        r.milestone,
        r.assignee,
        r.issue_type,
        r.jql,
        r.ado_query,
    ]);
    Sha256::digest(sel.to_string().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn needs_digest(r: &Rule) -> String {
    Sha256::digest(serde_json::json!(r.label_needs).to_string().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}
