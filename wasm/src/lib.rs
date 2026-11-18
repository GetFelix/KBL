//! The work fold from `kbl-core` as WebAssembly, for the dashboard.
//!
//! The page feeds it the payloads it reads from `work.events` and renders what
//! it returns. It adds no rules of its own: the state is the same fold the
//! tracker runs, so the two cannot disagree about an item's stage.

mod activity;

use std::cell::RefCell;
use std::collections::BTreeMap;

use kbl_core::content::Content;
use kbl_core::names::ItemId;
use kbl_core::overlap::{FILES_ENTRY, files_offset, overlaps};
use kbl_core::story::{Kept, accepted, has_story, story};
use kbl_core::work::{ReadError, WorkKind, WorkLogged, WorkRecord, read};
use kbl_core::work_fold::{Stage, State};
use serde_json::json;
use wasm_bindgen::prelude::*;

/// A fold in progress. Offsets are numbers, exact up to 2^53 like the gateway's.
#[wasm_bindgen]
#[derive(Default)]
pub struct Folder {
    state: State,
    /// The format of the first record that was newer than this build reads.
    newer: Option<u32>,
    /// The text of each item's current `files` entry, by its offset. The
    /// state keeps only the offset, and the page has no log to read it from.
    files: BTreeMap<u64, String>,
    /// `overlaps_json` as of the last applied record. The comparison is
    /// pairwise, so a redraw with no new record reuses it.
    overlaps: RefCell<Option<String>>,
    /// What the story reads of each decision, step, edit and note the fold
    /// took, by offset. The state keeps only the offsets.
    kept: BTreeMap<u64, Kept>,
    /// How many records of each item the fold has taken. A story is rebuilt
    /// only when its item's count moved.
    revs: BTreeMap<ItemId, u64>,
    /// Each item's story as serialised JSON (with its `rev`), by the count it
    /// was built at. None is an item with no story yet.
    stories: RefCell<BTreeMap<ItemId, (u64, Option<String>)>>,
    /// What happened on each item, for the panel's timeline and history.
    activity: activity::Activity,
}

#[wasm_bindgen]
impl Folder {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Folder {
        Folder::default()
    }

    /// Applies the record at `offset`. Returns false when the payload is not a
    /// record this build reads, or `offset` is not past the last one applied;
    /// the state is then unchanged. After a record of a newer format nothing
    /// more is applied: see [`Folder::newer`].
    pub fn apply(&mut self, offset: f64, payload: &[u8]) -> bool {
        let offset = offset as u64;
        if self.newer.is_some() || self.state.at.is_some_and(|at| offset <= at) {
            return false;
        }
        match read(payload) {
            Ok(rec) => {
                let before = self.files_offset(&rec);
                let spent = |s: &State| {
                    rec.record
                        .item()
                        .and_then(|i| s.items.get(i))
                        .map_or(0, |i| i.cost_micros)
                };
                let cost_before = spent(&self.state);
                self.state.apply(offset, &rec);
                let charged = spent(&self.state).saturating_sub(cost_before);
                // `apply` notes a refusal in `rejections` at this offset; the panel reads the same verdict.
                let refused = self
                    .state
                    .rejections
                    .last()
                    .filter(|r| r.offset == offset)
                    .map(|r| r.why);
                self.activity.note(offset, &rec, charged, refused);
                self.keep_files(offset, &rec, before);
                if accepted(&self.state, offset, &rec)
                    && let Some(k) = Kept::of(&rec)
                {
                    self.kept.insert(offset, k);
                }
                self.overlaps.take();
                if let Some(item) = rec.record.item() {
                    *self.revs.entry(item.clone()).or_default() += 1;
                }
                true
            }
            Err(ReadError::Newer { v }) => {
                self.newer = Some(v);
                false
            }
            Err(ReadError::Malformed(_)) => false,
        }
    }

    /// The format of the record that stopped the fold because this build is
    /// too old to read it, or 0. The page must stop and ask for a reload:
    /// the state no longer follows the log.
    pub fn newer(&self) -> u32 {
        self.newer.unwrap_or(0)
    }

    /// The log's format so far: the highest a record may have. The page hides
    /// the writes a log below that format would refuse.
    pub fn format(&self) -> u32 {
        self.state.format()
    }

    /// The last offset applied, or -1 before the first.
    pub fn at(&self) -> f64 {
        self.state.at.map_or(-1.0, |at| at as f64)
    }

    /// The whole state, as the tracker would serialize it.
    pub fn state_json(&self) -> String {
        serde_json::to_string(&self.state).expect("plain data serializes")
    }

    /// Every item as `{id, title, stage, tier, proposed_tier, source, repo, needs,
    /// question, options, default, unclear, root}`, in id order. `tier`, `source` and
    /// `repo` are null unless recorded: `tier` by acceptance or a tier change,
    /// `proposed_tier` by triage, `source` and `repo` by the item's target, or its
    /// root's target or import. `needs` says why the item waits on a person;
    /// `waiting_at` is the offset where it began waiting for that reason, or null.
    /// `question` is the offset of its open question, with that question's `options`
    /// and `default`, and `unclear` (`scope`, `repo` or `acceptance`) when it is
    /// triage's. `root` is true for an item with no parent. `accepted` and `flagged` say
    /// whether a person accepted it and whether one flagged it for a person.
    /// `triage` is the stage of a root's triage child, or null.
    /// `block` is what a person blocked it on (`until_ms`, `resource`, `reason`, `at_ms`),
    /// or null; one with an `until_ms` in the past no longer holds, which the page checks
    /// against its own clock. `assignee` is who works it (`{"person": k}`), or null; `held_attempt` is
    /// the attempt an agent holds, or null; `holder` and `active_ms` name that
    /// agent and when it last wrote. `parent`, `work`, `upstream` (the root's
    /// `owner/repo#number`), `pr`, `pr_phase`, `pr_checks` and `failing_check`
    /// let a card nest and describe itself. `hold_deadline_ms` is when the hold lapses
    /// unless the holder renews it.
    pub fn items_json(&self) -> String {
        let imported = self.state.imported_sources();
        let mut upstreams = std::collections::BTreeMap::new();
        for (_, id, root) in self.state.imported() {
            upstreams.entry(root).or_insert(id);
        }
        let items: Vec<_> = self
            .state
            .items
            .values()
            .map(|i| {
                let root = self.state.items.get(&i.root);
                let target = i.target.as_ref().or(root.and_then(|r| r.target.as_ref()));
                let source = target
                    .map(|t| t.source.as_str())
                    .or_else(|| imported.get(&i.root).copied());
                let upstream = upstreams.get(&i.root).copied();
                let pr = i.pr_status.as_ref();
                // The state of a root's triage child, so a card can show why a root is not fixable yet.
                let triage = i
                    .children
                    .iter()
                    .filter_map(|c| self.state.items.get(c))
                    .find(|c| c.work == WorkKind::Triage)
                    .map(|c| self.state.stage(&c.id));
                json!({
                    "id": i.id,
                    "title": i.title,
                    "parent": i.parent,
                    "work": i.work,
                    "upstream": upstream,
                    "pr": i.pr,
                    "pr_phase": pr.map(|p| p.phase),
                    "pr_checks": pr.map(|p| p.checks),
                    "failing_check": pr.and_then(|p| p.failing.as_ref()).map(|f| &f.name),
                    "holder": i.hold.as_ref().map(|h| &h.by),
                    "active_ms": i.hold.as_ref().map(|h| h.active_ms),
                    "stage": self.state.stage(&i.id),
                    "tier": i.tier,
                    "proposed_tier": i.proposal.as_ref().map(|p| p.tier),
                    "source": source,
                    "repo": target.map(|t| &t.repo),
                    "needs": self.state.needs_you(&i.id),
                    "question": i.open_question,
                    "waiting_at": i.waiting_since.map(|w| w.at),
                    "options": i.open_question.and(i.ask.as_ref()).map(|a| &a.options),
                    "default": i.open_question.and(i.ask.as_ref()).and_then(|a| a.default.as_ref()),
                    "unclear": i.open_question.and(i.ask.as_ref()).and_then(|a| a.unclear),
                    "root": i.parent.is_none(),
                    "accepted": i.accepted,
                    "flagged": i.flagged,
                    "block": i.block,
                    "assignee": i.assignee,
                    "held_attempt": i.hold.as_ref().map(|h| h.attempt),
                    "hold_deadline_ms": i.hold.as_ref().map(|h| h.deadline_ms),
                    "finding": i.finding,
                    "proposal": i.proposal.as_ref().map(|p| json!({
                        "at": p.at,
                        "why": p.why,
                        "depends_on": p.depends_on,
                        "split": p.split,
                    })),
                    "triage": triage,
                    "triage_skipped": i.triage_skipped,
                })
            })
            .collect();
        serde_json::to_string(&items).expect("plain data serializes")
    }

    /// What the review limit counts, as `{load, in_review}`: `load` is
    /// [`State::review_load`], the number the tracker's pause compares to the
    /// limit, and `in_review` the part of it that sits in In review. The rest of
    /// `load` is fixes agents hold.
    pub fn review_load_json(&self) -> String {
        json!({
            "load": self.state.review_load(),
            "in_review": self.state.in_review_count(),
        })
        .to_string()
    }

    /// Every item in the Blocked stage, as `{id, title, repo, source, waits_on}`, in
    /// id order. `waits_on` lists the items holding it, each with its own
    /// `repo`, stage, `source` and `awaiting_release` (merged, but the link
    /// waits for the release), so a blocker in another repo or source shows
    /// where it lives. `repo` is `{source, repo}`, or null when the fold knows none.
    pub fn blocked_json(&self) -> String {
        let s = &self.state;
        let source_of = |id: &ItemId| {
            let root = &s.items.get(id)?.root;
            s.imported()
                .find(|(_, _, r)| *r == root)
                .map(|(src, _, _)| src)
        };
        let blocker = |id: &ItemId| {
            json!({
                "id": id,
                "title": s.items.get(id).map(|i| i.title.as_str()),
                "repo": s.repo_of(id),
                "stage": s.stage(id),
                "source": source_of(id),
                "awaiting_release": s.items.get(id).is_some_and(|i| i.closed.is_some()),
            })
        };
        let blocked: Vec<_> = s
            .items
            .values()
            .filter(|i| i.closed.is_none() && s.stage(&i.id) == Some(Stage::Blocked))
            .filter_map(|i| {
                let by = s.blockers(&i.id);
                (!by.is_empty()).then(|| {
                    json!({
                        "id": i.id,
                        "title": i.title,
                        "repo": s.repo_of(&i.id),
                        "source": source_of(&i.id),
                        "waits_on": by.into_iter().map(blocker).collect::<Vec<_>>(),
                    })
                })
            })
            .collect();
        serde_json::to_string(&blocked).expect("plain data serializes")
    }

    /// Open items that expect to touch the same files, as
    /// `[{first, then, files}]`: `then` is suggested to wait on `first`.
    pub fn overlaps_json(&self) -> String {
        self.overlaps
            .borrow_mut()
            .get_or_insert_with(|| {
                let found = overlaps(&self.state, |o| self.files.get(&o).map(String::as_str));
                serde_json::to_string(&found).expect("plain data serializes")
            })
            .clone()
    }

    /// The story of every item with a recorded decision, as `{item: story}`:
    /// `rev` (changes whenever the item takes a record), `groups` (decisions by
    /// phase, each with its `edits` and `evidence`), `people`, `attempts` and
    /// `flags`. See `kbl_core::story`. A story is rebuilt only after its item
    /// takes a record.
    pub fn stories_json(&self) -> String {
        let mut cache = self.stories.borrow_mut();
        let mut out = String::from("{");
        for i in self.state.items.values().filter(|i| has_story(i)) {
            let rev = self.revs.get(&i.id).copied().unwrap_or(0);
            let stale = cache.get(&i.id).is_none_or(|(at, _)| *at != rev);
            if stale {
                let built = story(&self.state, &i.id, |o| self.kept.get(&o)).map(|st| {
                    let mut v = serde_json::to_value(&st).expect("plain data serializes");
                    v["rev"] = json!(rev);
                    v.to_string()
                });
                cache.insert(i.id.clone(), (rev, built));
            }
            if let Some((_, Some(text))) = cache.get(&i.id) {
                if out.len() > 1 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(&i.id).expect("an id is a string"));
                out.push(':');
                out.push_str(text);
            }
        }
        out.push('}');
        out
    }

    /// What happened on one item, oldest first, as `[{offset, at_ms, by, kind, attempt?,
    /// cost_micros?, ...}]`. `kind` is one of `claimed`, `progress`, `step` (with `command`,
    /// `exit_code` and the tail of `output`), `edit`, `decision`, `question`, `answer`,
    /// `note`, `done`, `released`, `gave_up`, `attempt_closed`, `rated`, `accepted`, `tier`,
    /// `taken`, `reassigned`, `closed` and `reopened`. `cost_micros` is what the fold charged
    /// the item for that record. A record the fold refused carries `refused` (the reason, such
    /// as `bad_usage`): it did not take effect, so the page shows it as refused and counts it nowhere.
    /// Read from records already applied; nothing is added to the log.
    pub fn activity_json(&self, item: &str) -> String {
        match ItemId::new(item) {
            Ok(id) => self.activity.json(&id),
            Err(_) => "[]".to_owned(),
        }
    }

    /// The latest finished attempt of each item, as `{item: {attempt, done_at, summary,
    /// files, added, removed, partial, failed_steps, failed_total, accurate}}`: the
    /// last progress note, the files and diff lines its recorded edits touch (`partial`
    /// when a hunk is stored as an artifact), its failed steps, and the standing mark.
    /// Only records the fold accepted count.
    pub fn handoffs_json(&self) -> String {
        self.activity.handoffs_json()
    }

    /// Claim costs summed per root, reviewer tier and repo: `Spend` from the core fold.
    pub fn spend_json(&self) -> String {
        serde_json::to_string(&self.state.spend()).expect("plain data serializes")
    }
}

impl Folder {
    /// The offset of the `files` entry `rec` would replace, if it writes one.
    fn files_offset(&self, rec: &WorkLogged) -> Option<u64> {
        match &rec.record {
            WorkRecord::Context { item, entry, .. } if entry.as_str() == FILES_ENTRY => {
                files_offset(self.state.items.get(item)?)
            }
            _ => None,
        }
    }

    /// Keeps the text of a `files` entry the fold took, and drops the one it replaced.
    fn keep_files(&mut self, offset: u64, rec: &WorkLogged, before: Option<u64>) {
        let WorkRecord::Context { item, entry, text } = &rec.record else {
            return;
        };
        let now = self.state.items.get(item).and_then(files_offset);
        if entry.as_str() != FILES_ENTRY || now != Some(offset) {
            return;
        }
        if let Some(old) = before {
            self.files.remove(&old);
        }
        if let Content::Text(t) = text {
            self.files.insert(offset, t.clone());
        }
    }
}
