//! Test doubles: GitHub answers recorded in `fixtures/` and replayed by URL,
//! and a tracker that runs the real fold. Nothing here reaches the network.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use kbl_core::names::{ItemId, Key};
use kbl_core::work::{Actor, WorkLogged, WorkRecord};
use kbl_core::work_fold::State;
use serde_json::{Value, json};

use crate::github::{Req, Res, Transport};
use crate::tracker_api::{Appended, Flake, Listed, Prerequisite, Root, Saved, Tracker};

/// The Fake tracker's clock at offset zero: 2027-01-15T08:00:00Z.
pub(crate) const CLOCK_START_MS: u64 = 1_800_000_000_000;

pub(crate) fn fixture(name: &str) -> Value {
    let path = format!("{}/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect(&path)
}

/// The recorded issue with fields replaced.
pub(crate) fn issue(number: u64, edit: impl FnOnce(&mut Value)) -> Value {
    let mut v = fixture("issue");
    v["number"] = json!(number);
    v["html_url"] = json!(format!("https://github.com/GetFelix/felix/issues/{number}"));
    edit(&mut v);
    v
}

pub(crate) fn labels(v: &mut Value, names: &[&str]) {
    v["labels"] = names.iter().map(|n| json!({ "name": n })).collect();
}

pub(crate) fn ok(body: Value) -> Res {
    Res {
        status: 200,
        headers: BTreeMap::new(),
        body,
    }
}

pub(crate) fn status(code: u16, headers: &[(&str, &str)]) -> Res {
    Res {
        status: code,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        body: Value::Null,
    }
}

/// An answer that never comes: the lease fence cut the request after it
/// was sent, so it may have landed.
pub(crate) fn cut() -> Res {
    status(0, &[])
}

struct Route {
    method: &'static str,
    contains: String,
    answers: VecDeque<Res>,
}

#[derive(Default)]
struct Net {
    routes: Vec<Route>,
    sent: Vec<Req>,
}

/// GitHub as recorded. The last answer of a route repeats; a URL with no route gets a 404.
#[derive(Clone, Default)]
pub(crate) struct Script(Arc<Mutex<Net>>);

impl Script {
    /// Answers requests whose URL contains `contains`, replacing earlier answers for the same pair.
    pub(crate) fn on(&self, method: &'static str, contains: &str, answer: Res) {
        let mut net = self.0.lock().unwrap();
        net.routes
            .retain(|r| !(r.method == method && r.contains == contains));
        net.routes.push(Route {
            method,
            contains: contains.to_owned(),
            answers: VecDeque::from([answer]),
        });
    }

    /// Like `on`, with answers given in turn; the last one repeats.
    pub(crate) fn on_seq(&self, method: &'static str, contains: &str, answers: Vec<Res>) {
        let mut net = self.0.lock().unwrap();
        net.routes
            .retain(|r| !(r.method == method && r.contains == contains));
        net.routes.push(Route {
            method,
            contains: contains.to_owned(),
            answers: VecDeque::from(answers),
        });
    }

    pub(crate) fn sent(&self) -> Vec<Req> {
        self.0.lock().unwrap().sent.clone()
    }

    /// Requests that change something on GitHub.
    pub(crate) fn writes(&self) -> Vec<String> {
        self.sent()
            .iter()
            .filter(|r| r.method != "GET")
            .map(|r| format!("{} {}", r.method, r.url))
            .collect()
    }
}

impl Transport for Script {
    async fn send(&self, req: Req) -> Result<Res> {
        let mut net = self.0.lock().unwrap();
        net.sent.push(req.clone());
        let best = net
            .routes
            .iter_mut()
            .filter(|r| r.method == req.method && req.url.contains(&r.contains))
            .max_by_key(|r| r.contains.len());
        let res = match best {
            Some(r) if r.answers.len() > 1 => r.answers.pop_front().unwrap(),
            Some(r) => r.answers[0].clone(),
            None => status(404, &[]),
        };
        if res.status == 0 {
            return Err(crate::lease::FenceClosed("request cancelled".into()).into());
        }
        Ok(res)
    }
}

struct Inner {
    log: Vec<(u64, WorkLogged)>,
    state: State,
    saved: Option<(Value, u64)>,
    save_fails: bool,
    /// Every append fails, as when the tracker cannot be reached.
    append_fails: bool,
    /// Appends of this record kind fail; every other kind lands.
    fail_kind: Option<&'static str>,
    /// The roots omit `closed_by_merge`, as a tracker that predates it.
    old_tracker: bool,
    roots_calls: usize,
    race_next_save: bool,
    saves_left: Option<usize>,
    /// Writes (appends and saves) that still land before the process dies;
    /// every later one fails, as if it had stopped between two writes.
    writes_left: Option<usize>,
    /// The next append lands and its reply is lost.
    lose_next_append_reply: bool,
    /// The edges ingest's own `linked` made, as the tracker tracks them.
    ingest_edges: HashSet<(ItemId, ItemId)>,
    /// `linked` records ingest sent, written or refused.
    links_sent: usize,
    /// Appends the fold refused.
    refused: usize,
    /// The roots listing shows closed items as open.
    roots_show_open: bool,
}

/// A tracker that folds what it is given, so a record the real fold would refuse is refused here.
#[derive(Clone)]
pub(crate) struct Fake(Arc<Mutex<Inner>>);

impl Default for Fake {
    fn default() -> Self {
        Fake(Arc::new(Mutex::new(Inner {
            log: Vec::new(),
            state: State::default(),
            saved: None,
            save_fails: false,
            append_fails: false,
            fail_kind: None,
            old_tracker: false,
            roots_calls: 0,
            race_next_save: false,
            saves_left: None,
            writes_left: None,
            lose_next_append_reply: false,
            ingest_edges: HashSet::new(),
            links_sent: 0,
            refused: 0,
            roots_show_open: false,
        })))
    }
}

impl Fake {
    fn push(&self, by: &str, actor: Actor, record: WorkRecord) -> Result<(), String> {
        let mut i = self.0.lock().unwrap();
        let offset = i.log.len() as u64;
        let logged = WorkLogged::new(
            CLOCK_START_MS + offset,
            Key::new(by).unwrap(),
            actor,
            record,
        );
        let absent = match &logged.record {
            WorkRecord::Linked { item, to } => !i.state.edges.contains(&(item.clone(), to.clone())),
            _ => false,
        };
        i.state.try_apply(offset, &logged).map_err(|r| {
            serde_json::to_value(r)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })?;
        match &logged.record {
            WorkRecord::Linked { item, to } if by == "ingest-gh" => {
                if absent {
                    i.ingest_edges.insert((item.clone(), to.clone()));
                }
            }
            WorkRecord::Linked { item, to } | WorkRecord::Unlinked { item, to } => {
                i.ingest_edges.remove(&(item.clone(), to.clone()));
            }
            WorkRecord::Prerequisite {
                item, prerequisite, ..
            } => {
                i.ingest_edges.remove(&(item.clone(), prerequisite.clone()));
            }
            _ => {}
        }
        i.log.push((offset, logged));
        Ok(())
    }

    /// An agent's record, which must be accepted.
    pub(crate) fn agent(&self, by: &str, record: WorkRecord) {
        self.push(by, Actor::Agent, record).expect("accepted");
    }

    /// A person's record, which must be accepted.
    pub(crate) fn person(&self, record: WorkRecord) {
        self.push("gabriel", Actor::Person, record)
            .expect("accepted");
    }

    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        self.0
            .lock()
            .unwrap()
            .log
            .iter()
            .map(|(_, l)| l.record.kind())
            .collect()
    }

    pub(crate) fn records(&self) -> Vec<WorkRecord> {
        let inner = self.0.lock().unwrap();
        inner.log.iter().map(|(_, l)| l.record.clone()).collect()
    }

    pub(crate) fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| **k == kind).count()
    }

    /// The roots listing shows closed items as open, as a tracker that
    /// lags the log would.
    pub(crate) fn roots_show_closed_as_open(&self) {
        self.0.lock().unwrap().roots_show_open = true;
    }

    /// Appends from ingest that the fold refused.
    pub(crate) fn refused(&self) -> usize {
        self.0.lock().unwrap().refused
    }

    pub(crate) fn state(&self) -> State {
        self.0.lock().unwrap().state.clone()
    }

    /// The item imported for an upstream id.
    pub(crate) fn item_of(&self, upstream: &str) -> ItemId {
        self.state()
            .imported()
            .find(|(_, u, _)| *u == upstream)
            .map(|(_, _, i)| i.clone())
            .unwrap_or_else(|| panic!("{upstream} is not imported"))
    }

    /// The ingest state as last saved.
    pub(crate) fn saved_state(&self) -> crate::state::State {
        let i = self.0.lock().unwrap();
        serde_json::from_value(i.saved.clone().expect("saved").0).unwrap()
    }

    /// The ingest state as last saved, if it was saved at all.
    pub(crate) fn try_saved(&self) -> Option<crate::state::State> {
        let i = self.0.lock().unwrap();
        i.saved
            .as_ref()
            .map(|(v, _)| serde_json::from_value(v.clone()).unwrap())
    }

    /// Saved state written by an older version, in its own form.
    pub(crate) fn seed_saved(&self, state: Value) {
        self.0.lock().unwrap().saved = Some((state, 1));
    }

    /// The saved state as JSON, exactly as written.
    pub(crate) fn saved_raw(&self) -> Value {
        self.0.lock().unwrap().saved.clone().expect("saved").0
    }

    /// Another ingest process saves just before this one's next save, so
    /// that save is stale.
    pub(crate) fn race_next_save(&self) {
        self.0.lock().unwrap().race_next_save = true;
    }

    /// The next `n` saves work and every one after is refused.
    pub(crate) fn fail_saves_after(&self, n: usize) {
        self.0.lock().unwrap().saves_left = Some(n);
    }

    /// Every later save is refused.
    pub(crate) fn fail_saves(&self) {
        self.0.lock().unwrap().save_fails = true;
    }

    /// Saves work again after `fail_saves`.
    pub(crate) fn allow_saves(&self) {
        let mut i = self.0.lock().unwrap();
        i.save_fails = false;
        i.saves_left = None;
    }

    /// The process dies after `n` more writes. `restart` brings it back.
    pub(crate) fn crash_after_writes(&self, n: usize) {
        self.0.lock().unwrap().writes_left = Some(n);
    }

    /// Every pending fault is gone.
    pub(crate) fn restart(&self) {
        let mut i = self.0.lock().unwrap();
        i.writes_left = None;
        i.lose_next_append_reply = false;
        i.race_next_save = false;
    }

    /// The next append lands, and ingest is told it failed.
    pub(crate) fn lose_next_append_reply(&self) {
        self.0.lock().unwrap().lose_next_append_reply = true;
    }

    /// A person's record, if the fold accepts it.
    pub(crate) fn try_person(&self, record: WorkRecord) -> bool {
        self.push("gabriel", Actor::Person, record).is_ok()
    }

    /// Whether every later append fails, as if the tracker went away.
    pub(crate) fn fail_appends(&self, fail: bool) {
        self.0.lock().unwrap().append_fails = fail;
    }

    /// Appends of one record kind fail, as if the tracker went away between
    /// two writes. `None` lifts it.
    pub(crate) fn fail_kind(&self, kind: Option<&'static str>) {
        self.0.lock().unwrap().fail_kind = kind;
    }

    /// The roots lack `closed_by_merge`, as from a tracker that predates it.
    pub(crate) fn act_as_old_tracker(&self) {
        self.0.lock().unwrap().old_tracker = true;
    }

    /// Ingest's saved state is lost, as if the cache entry expired.
    pub(crate) fn forget_saved(&self) {
        self.0.lock().unwrap().saved = None;
    }
}

impl Fake {
    fn spend_write(&self) -> Result<()> {
        let mut i = self.0.lock().unwrap();
        if let Some(left) = i.writes_left.as_mut() {
            anyhow::ensure!(*left > 0, "the process died");
            *left -= 1;
        }
        Ok(())
    }

    /// How many times ingest listed the roots.
    pub(crate) fn links_sent(&self) -> usize {
        self.0.lock().unwrap().links_sent
    }

    pub(crate) fn roots_calls(&self) -> usize {
        self.0.lock().unwrap().roots_calls
    }

    fn root_list(&self) -> Vec<Root> {
        let i = self.0.lock().unwrap();
        i.state
            .imported()
            .map(|(source, up, item)| {
                let it = &i.state.items[item];
                Root {
                    item: item.clone(),
                    source: source.to_owned(),
                    upstream_id: up.to_owned(),
                    stage: serde_json::to_value(i.state.stage(item))
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    closed: it.closed.is_some() && !i.roots_show_open,
                    closed_by_sync: it.closed_by_sync,
                    closed_by_merge: (!i.old_tracker)
                        .then_some(it.closed == Some(kbl_core::work_fold::Closure::Merged)),
                    left_scope_by_sync: it.left_scope_by_sync,
                    reopened_by: it.reopened_by,
                    has_children: !it.children.is_empty(),
                    pr: it.pr.clone(),
                    claimed_at_ms: it.first_claim_ms,
                    pr_status: it.pr_status.clone(),
                    release: it.release.clone(),
                    awaits_release: i.state.awaits_release(item),
                }
            })
            .collect()
    }

    fn prerequisite_list(&self) -> Vec<Prerequisite> {
        let i = self.0.lock().unwrap();
        let entry = Key::new("prerequisite").unwrap();
        i.state
            .unopened_prerequisites()
            .into_iter()
            .map(|(it, needed_by)| {
                let target = it.target.clone().unwrap();
                let why = it.context.get(&entry).and_then(|at| {
                    match &i.log.get(*at as usize)?.1.record {
                        WorkRecord::Prerequisite { new: Some(n), .. } => match &n.why {
                            kbl_core::content::Content::Text(t) => Some(t.clone()),
                            _ => None,
                        },
                        _ => None,
                    }
                });
                Prerequisite {
                    item: it.id.clone(),
                    title: it.title.clone(),
                    source: target.source,
                    repo: target.repo,
                    why,
                    needed_by: needed_by.map(str::to_owned),
                }
            })
            .collect()
    }
}

impl Tracker for Fake {
    async fn roots(&self) -> Result<Listed> {
        self.0.lock().unwrap().roots_calls += 1;
        Ok(Listed {
            roots: self.root_list(),
            prerequisites: self.prerequisite_list(),
            flakes: self
                .state()
                .flakes
                .iter()
                .map(|(name, f)| Flake {
                    flake: name.clone(),
                    check: f.check.clone(),
                    pattern: f.pattern.clone(),
                })
                .collect(),
        })
    }

    async fn append(&self, record: &WorkRecord) -> Result<Appended> {
        anyhow::ensure!(
            !self.0.lock().unwrap().append_fails,
            "the tracker is unreachable"
        );
        anyhow::ensure!(
            self.0.lock().unwrap().fail_kind != Some(record.kind()),
            "the tracker is unreachable"
        );
        self.spend_write()?;
        // The tracker's ingest gate: ingest links and unlinks only edges its
        // own link made.
        if let WorkRecord::Linked { item, to } | WorkRecord::Unlinked { item, to } = record {
            let mut i = self.0.lock().unwrap();
            if matches!(record, WorkRecord::Linked { .. }) {
                i.links_sent += 1;
            }
            let edge = (item.clone(), to.clone());
            let ours = i.ingest_edges.contains(&edge);
            match (record, i.state.edges.contains(&edge)) {
                (WorkRecord::Linked { .. }, true) if !ours => {
                    return Ok(Appended::Refused("already_linked".into()));
                }
                (WorkRecord::Unlinked { .. }, false) => {
                    return Ok(Appended::Refused("not_linked".into()));
                }
                (WorkRecord::Unlinked { .. }, true) if !ours => {
                    return Ok(Appended::Refused("not_ingests".into()));
                }
                _ => {}
            }
        }
        let pushed = self.push("ingest-gh", Actor::Tracker, record.clone());
        if pushed.is_ok() && std::mem::take(&mut self.0.lock().unwrap().lose_next_append_reply) {
            anyhow::bail!("the reply was lost");
        }
        Ok(match pushed {
            Ok(()) => Appended::Written,
            Err(why) => {
                self.0.lock().unwrap().refused += 1;
                Appended::Refused(why)
            }
        })
    }

    async fn load(&self, _: &str) -> Result<Saved> {
        let i = self.0.lock().unwrap();
        Ok(match &i.saved {
            Some((v, ver)) => (Some(v.clone()), Some(*ver)),
            None => (None, None),
        })
    }

    async fn save(&self, _: &str, state: &Value, version: Option<u64>) -> Result<u64> {
        self.spend_write()?;
        let mut i = self.0.lock().unwrap();
        anyhow::ensure!(!i.save_fails, "the tracker refused the save");
        if let Some(left) = i.saves_left.as_mut() {
            anyhow::ensure!(*left > 0, "the tracker refused the save");
            *left -= 1;
        }
        if std::mem::take(&mut i.race_next_save) {
            let now = i.saved.as_ref().map_or(0, |(_, v)| *v);
            let other = i.saved.as_ref().map_or_else(
                || serde_json::to_value(crate::state::State::default()).unwrap(),
                |(s, _)| s.clone(),
            );
            i.saved = Some((other, now + 1));
        }
        let now = i.saved.as_ref().map(|(_, v)| *v);
        anyhow::ensure!(now == version, "stale_state");
        let next = now.unwrap_or(0) + 1;
        i.saved = Some((state.clone(), next));
        Ok(next)
    }
}
