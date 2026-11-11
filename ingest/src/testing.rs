//! Test doubles: GitHub answers recorded in `fixtures/` and replayed by URL,
//! and a tracker that runs the real fold. Nothing here reaches the network.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use agents_core::names::{ItemId, Key};
use agents_core::work::{Actor, WorkLogged, WorkRecord};
use agents_core::work_fold::State;
use anyhow::Result;
use serde_json::{Value, json};

use crate::github::{Req, Res, Transport};
use crate::tracker_api::{Appended, Root, Saved, Tracker};

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
        Ok(match best {
            Some(r) if r.answers.len() > 1 => r.answers.pop_front().unwrap(),
            Some(r) => r.answers[0].clone(),
            None => status(404, &[]),
        })
    }
}

struct Inner {
    log: Vec<(u64, WorkLogged)>,
    state: State,
    saved: Option<(Value, u64)>,
    save_fails: bool,
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
        })))
    }
}

impl Fake {
    fn push(&self, by: &str, actor: Actor, record: WorkRecord) -> Result<(), String> {
        let mut i = self.0.lock().unwrap();
        let offset = i.log.len() as u64;
        let logged = WorkLogged {
            at_ms: offset,
            by: Key::new(by).unwrap(),
            actor,
            record,
        };
        i.state.try_apply(offset, &logged).map_err(|r| {
            serde_json::to_value(r)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })?;
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

    pub(crate) fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| **k == kind).count()
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

    /// Saved state written by an older version, in its own form.
    pub(crate) fn seed_saved(&self, state: Value) {
        self.0.lock().unwrap().saved = Some((state, 1));
    }

    /// The saved state as JSON, exactly as written.
    pub(crate) fn saved_raw(&self) -> Value {
        self.0.lock().unwrap().saved.clone().expect("saved").0
    }

    /// Every later save is refused.
    pub(crate) fn fail_saves(&self) {
        self.0.lock().unwrap().save_fails = true;
    }

    /// Ingest's saved state is lost, as if the cache entry expired.
    pub(crate) fn forget_saved(&self) {
        self.0.lock().unwrap().saved = None;
    }
}

impl Tracker for Fake {
    async fn roots(&self) -> Result<Vec<Root>> {
        let i = self.0.lock().unwrap();
        Ok(i.state
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
                    closed: it.closed.is_some(),
                    pr: it.pr.clone(),
                    branch: it.branch.clone(),
                    pr_status: it.pr_status.clone(),
                }
            })
            .collect())
    }

    async fn append(&self, record: &WorkRecord) -> Result<Appended> {
        Ok(
            match self.push("ingest-gh", Actor::Tracker, record.clone()) {
                Ok(()) => Appended::Written,
                Err(why) => Appended::Refused(why),
            },
        )
    }

    async fn load(&self, _: &str) -> Result<Saved> {
        let i = self.0.lock().unwrap();
        Ok(match &i.saved {
            Some((v, ver)) => (Some(v.clone()), Some(*ver)),
            None => (None, None),
        })
    }

    async fn save(&self, _: &str, state: &Value, version: Option<u64>) -> Result<u64> {
        let mut i = self.0.lock().unwrap();
        anyhow::ensure!(!i.save_fails, "the tracker refused the save");
        let now = i.saved.as_ref().map(|(_, v)| *v);
        anyhow::ensure!(now == version, "stale_state");
        let next = now.unwrap_or(0) + 1;
        i.saved = Some((state.clone(), next));
        Ok(next)
    }
}
