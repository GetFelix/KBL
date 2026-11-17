//! `PUT /v1/ingest/leases`: a worker's heartbeat, which renews the source
//! leases it holds and takes the ones rendezvous hashing gives it once they are
//! free. Who is live is kept in memory: a lease itself is only ever moved by a
//! version-checked write, so two trackers that disagree on who is live cost a
//! slower handover, never two holders.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::response::IntoResponse as _;
use futures::StreamExt as _;
use kbl_core::lease::{self, FENCE_MS, HEARTBEAT_MS, LEASE_MS, Lease, Step};
use kbl_core::names::Key;
use kbl_core::org::TeamName;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use super::{App, Arc, Call, Json, Response, State, StatusCode, denied, refused};
use crate::felix::Broker;
use crate::tracker::Tracker;

/// Live workers one group may have. Past it a new worker is refused, so a
/// token cannot grow the table without bound.
pub(crate) const MAX_MEMBERS: usize = 256;

/// Lease reads and writes one heartbeat has in flight.
const IN_FLIGHT: usize = 16;

/// The live workers of every group and the leases each last got.
#[derive(Default)]
pub(crate) struct Groups {
    groups: Mutex<HashMap<(TeamName, Key), Group>>,
    /// Added to the wall clock, so a test can let leases run out.
    pub(crate) skew_ms: AtomicU64,
}

struct Group {
    /// Worker to the time it was last heard from.
    seen: BTreeMap<String, u64>,
    /// Worker to the leases its last heartbeat answered.
    held: BTreeMap<String, Vec<(String, u64)>>,
    /// Bumped when either changes, to wake held config polls.
    changed: watch::Sender<u64>,
}

impl Default for Group {
    fn default() -> Self {
        Self {
            seen: BTreeMap::new(),
            held: BTreeMap::new(),
            changed: watch::Sender::new(0),
        }
    }
}

impl Group {
    fn prune(&mut self, now: u64) -> bool {
        let before = self.seen.len();
        self.seen.retain(|_, at| *at + LEASE_MS > now);
        self.held.retain(|w, _| self.seen.contains_key(w));
        before != self.seen.len()
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    fn assigned<'a>(&self, sources: &'a [String], me: &str) -> Vec<&'a String> {
        sources
            .iter()
            .filter(|s| lease::owner(s, self.seen.keys().map(String::as_str)) == Some(me))
            .collect()
    }
}

impl Groups {
    pub(crate) fn now_ms(&self) -> u64 {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        wall + self.skew_ms.load(Ordering::SeqCst)
    }

    /// What the config answer says about `me`: the sources it is assigned and
    /// the leases its last heartbeat got, with a receiver that changes when
    /// either may have.
    pub(super) fn view(
        &self,
        team: &TeamName,
        group: &Key,
        me: &str,
        sources: &[String],
    ) -> (Value, watch::Receiver<u64>) {
        let now = self.now_ms();
        let mut groups = self.groups.lock().expect("lease groups lock");
        let g = groups.entry((team.clone(), group.clone())).or_default();
        if g.prune(now) {
            g.bump();
        }
        let leases: Vec<Value> = g
            .held
            .get(me)
            .into_iter()
            .flatten()
            .map(|(s, e)| json!({ "source": s, "epoch": e }))
            .collect();
        let view = json!({ "assigned": g.assigned(sources, me), "leases": leases });
        (view, g.changed.subscribe())
    }
}

#[derive(Deserialize)]
pub(super) struct HeartbeatBody {
    group: String,
    /// Tells apart workers that share a token. Drawn fresh on every start.
    instance: String,
    /// The worker is stopping: release what it holds and leave the group.
    #[serde(default)]
    leave: bool,
}

/// The worker as leases and the group name it: its token's name and instance.
pub(super) fn member(worker: &Key, instance: &Key) -> String {
    format!("{worker}/{instance}")
}

pub(super) async fn heartbeat<B: Broker>(
    State(app): State<Arc<App<B>>>,
    call: Call<B>,
    Json(body): Json<HeartbeatBody>,
) -> Response {
    let worker = match call.worker() {
        Ok(w) => w,
        Err(d) => return denied(d),
    };
    let (Ok(group), Ok(instance)) = (Key::new(body.group), Key::new(body.instance)) else {
        return refused(
            StatusCode::BAD_REQUEST,
            "group and instance are keys",
            "invalid",
        );
    };
    let Some(org) = call.teams.org().map(|o| o.org.clone()) else {
        return refused(
            StatusCode::NOT_FOUND,
            "this tracker runs without teams: set KBL_ORG_NAMESPACE",
            "teams_off",
        );
    };
    let team = call.team.name().clone();
    let Some(sources) = org.with_view(|state, _| {
        super::connection_for(state, &team, &group, worker.source.as_deref())
            .map(|(_, c)| c.sources.clone())
    }) else {
        return super::super::org_unavailable();
    };
    // Only a connection of the worker's own team names the sources, and the
    // leases sit in that team's namespace: another team's are out of reach.
    let Some(sources) = sources else {
        return refused(
            StatusCode::CONFLICT,
            "no connection of this team names this group",
            "no_connection",
        );
    };
    let me = member(&worker.key, &instance);
    let tracker = call.tracker();
    let groups = &app.leases;
    if body.leave {
        let held: Vec<String> = groups
            .groups
            .lock()
            .expect("lease groups lock")
            .get(&(team.clone(), group.clone()))
            .and_then(|g| g.held.get(&me))
            .map(|h| h.iter().map(|(s, _)| s.clone()).collect())
            .unwrap_or_default();
        release_all(tracker, groups, &held, &me).await;
        if let Some(g) = groups
            .groups
            .lock()
            .expect("lease groups lock")
            .get_mut(&(team, group))
        {
            g.seen.remove(&me);
            g.held.remove(&me);
            g.bump();
        }
        return Json(json!({ "leases": [] })).into_response();
    }
    let now = groups.now_ms();
    let assigned: Vec<String> = {
        let mut all = groups.groups.lock().expect("lease groups lock");
        let g = all.entry((team.clone(), group.clone())).or_default();
        let mut moved = g.prune(now);
        if !g.seen.contains_key(&me) {
            if g.seen.len() >= MAX_MEMBERS {
                drop(all);
                return refused(
                    StatusCode::TOO_MANY_REQUESTS,
                    "this group has as many live workers as it may",
                    "too_many_workers",
                );
            }
            moved = true;
        }
        g.seen.insert(me.clone(), now);
        if moved {
            g.bump();
        }
        g.assigned(&sources, &me).into_iter().cloned().collect()
    };
    let who = me.as_str();
    let got: Vec<(String, u64)> = futures::stream::iter(assigned)
        .map(|s| async move {
            let epoch = take_or_renew(tracker, &s, who, now).await;
            epoch.map(|e| (s, e))
        })
        .buffer_unordered(IN_FLIGHT)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    let mut got = got;
    got.sort();
    if let Some(g) = groups
        .groups
        .lock()
        .expect("lease groups lock")
        .get_mut(&(team, group))
        && g.held.get(&me) != Some(&got)
    {
        g.held.insert(me.clone(), got.clone());
        g.bump();
    }
    let leases: Vec<Value> = got
        .iter()
        .map(|(s, e)| json!({ "source": s, "epoch": e }))
        .collect();
    Json(json!({
        "leases": leases,
        "lease_ms": LEASE_MS,
        "fence_ms": FENCE_MS,
        "heartbeat_ms": HEARTBEAT_MS,
    }))
    .into_response()
}

/// The epoch `me` holds `source` at after this heartbeat, or none. A failed
/// read or write counts as not held, which stops the worker polling it.
async fn take_or_renew<B: Broker>(
    tracker: &Tracker<B>,
    source: &str,
    me: &str,
    now: u64,
) -> Option<u64> {
    let current = tracker.source_lease(source).await.ok()?;
    let (lease, version) = match &current {
        Some((l, v)) => (Some(l), Some(*v)),
        None => (None, None),
    };
    let step = lease::step(lease, me, true, now);
    let next = lease::next(lease, me, step, now)?;
    debug_assert!(step != Step::Leave);
    match tracker.put_source_lease(source, &next, version).await {
        Ok(true) => Some(next.epoch),
        _ => None,
    }
}

/// Releases the leases `me` holds among `sources`, so the next holder need
/// not wait for them to expire.
async fn release_all<B: Broker>(
    tracker: &Tracker<B>,
    groups: &Groups,
    sources: &[String],
    me: &str,
) {
    let now = groups.now_ms();
    futures::stream::iter(sources)
        .for_each_concurrent(IN_FLIGHT, |s| async move {
            if let Ok(Some((l, v))) = tracker.source_lease(s).await
                && l.holder.as_deref() == Some(me)
            {
                let released = Lease {
                    holder: None,
                    epoch: l.epoch,
                    expires_ms: now,
                };
                let _ = tracker.put_source_lease(s, &released, Some(v)).await;
            }
        })
        .await;
}
