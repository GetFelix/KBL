//! `PUT /v1/ingest/leases`: a worker's heartbeat, which renews the source
//! leases it holds and takes the ones rendezvous hashing gives it once they are
//! free. Who is live is one cache entry per group, so every tracker computes
//! the same assignment; a lease itself only moves by a version-checked write,
//! so even trackers that read membership at different moments never make two
//! holders.

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

/// Tries at the membership entry before a heartbeat gives up with `503`.
const JOIN_TRIES: usize = 8;

/// This tracker's view of each group. The leases and membership themselves
/// live in the cache; this holds only what the config answer shows and the
/// owner map, each group behind its own lock.
#[derive(Default)]
pub(crate) struct Groups {
    groups: Mutex<HashMap<(TeamName, Key), Arc<Group>>>,
    /// Added to the wall clock, so a test can let leases run out.
    pub(crate) skew_ms: AtomicU64,
}

#[derive(Default)]
struct Group {
    view: Mutex<View>,
    /// Bumped when the view changes, to wake held config polls.
    changed: watch::Sender<u64>,
}

#[derive(Default)]
struct View {
    /// Live members as of the last membership read, sorted.
    members: Vec<String>,
    /// The connection's sources the owner map was computed for.
    sources: Vec<String>,
    /// Source to its rendezvous owner among `members`. Recomputed only when
    /// `members` or `sources` change, not on every heartbeat.
    owners: HashMap<String, String>,
    /// Worker to the leases its last heartbeat here answered.
    held: BTreeMap<String, Vec<(String, u64)>>,
}

impl View {
    fn refresh(&mut self, members: Vec<String>, sources: &[String]) -> bool {
        self.held.retain(|w, _| members.binary_search(w).is_ok());
        if self.members == members && self.sources == sources {
            return false;
        }
        self.owners = sources
            .iter()
            .filter_map(|s| {
                let o = lease::owner(s, members.iter().map(String::as_str))?;
                Some((s.clone(), o.to_owned()))
            })
            .collect();
        self.members = members;
        self.sources = sources.to_vec();
        true
    }

    fn assigned<'a>(&self, sources: &'a [String], me: &str) -> Vec<&'a String> {
        sources
            .iter()
            .filter(|s| self.owners.get(*s).map(String::as_str) == Some(me))
            .collect()
    }
}

/// The group's live members as the cache holds them: member to the tracker
/// time it last heartbeated.
type Members = BTreeMap<String, u64>;

fn live(members: &mut Members, now: u64) {
    members.retain(|_, seen| *seen + LEASE_MS > now);
}

impl Group {
    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }
}

impl Groups {
    pub(crate) fn now_ms(&self) -> u64 {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        wall + self.skew_ms.load(Ordering::SeqCst)
    }

    fn group(&self, team: &TeamName, group: &Key) -> Arc<Group> {
        self.groups
            .lock()
            .expect("lease groups lock")
            .entry((team.clone(), group.clone()))
            .or_default()
            .clone()
    }

    /// What the config answer says about `me`, from this tracker's last view:
    /// the sources it is assigned and the leases its last heartbeat here got,
    /// with a receiver that changes when either may have. Advisory only; the
    /// heartbeat answer is what the worker acts on. Reads nothing from Felix,
    /// so a held poll stays cheap.
    pub(super) fn view(
        &self,
        team: &TeamName,
        group: &Key,
        me: &str,
        sources: &[String],
    ) -> (Value, watch::Receiver<u64>) {
        let g = self.group(team, group);
        let view = g.view.lock().expect("lease view lock");
        let leases: Vec<Value> = view
            .held
            .get(me)
            .into_iter()
            .flatten()
            .map(|(s, e)| json!({ "source": s, "epoch": e }))
            .collect();
        let answer = json!({ "assigned": view.assigned(sources, me), "leases": leases });
        (answer, g.changed.subscribe())
    }
}

/// Why a membership write did not go through.
enum Join {
    Full,
    Failed,
}

/// Writes `me` into the group's membership at `now` (or out of it, when
/// `leave`), dropping members past their expiry, and answers who is live.
async fn join<B: Broker>(
    tracker: &Tracker<B>,
    group: &Key,
    me: &str,
    now: u64,
    leave: bool,
) -> Result<Members, Join> {
    for _ in 0..JOIN_TRIES {
        let Ok(current) = tracker.group_members(group).await else {
            return Err(Join::Failed);
        };
        let (mut members, version) = match current {
            Some((m, v)) => (m, Some(v)),
            None => (Members::new(), None),
        };
        live(&mut members, now);
        if leave {
            members.remove(me);
        } else {
            if !members.contains_key(me) && members.len() >= MAX_MEMBERS {
                return Err(Join::Full);
            }
            members.insert(me.to_owned(), now);
        }
        match tracker.put_group_members(group, &members, version).await {
            Ok(true) => return Ok(members),
            Ok(false) => continue,
            Err(_) => return Err(Join::Failed),
        }
    }
    Err(Join::Failed)
}

#[derive(Deserialize)]
pub(super) struct HeartbeatBody {
    group: String,
    /// Tells apart workers that share a token. Drawn fresh on every start.
    instance: String,
    /// The worker is stopping: release what it holds and leave the group.
    #[serde(default)]
    leave: bool,
    /// The sources the worker holds now. One no longer assigned to it is
    /// still answered while its lease is live, so it is not dropped early.
    #[serde(default)]
    held: Vec<String>,
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
    let g = groups.group(&team, &group);
    let now = groups.now_ms();
    let members = match join(tracker, &group, &me, now, body.leave).await {
        Ok(m) => m,
        Err(Join::Full) => {
            return refused(
                StatusCode::TOO_MANY_REQUESTS,
                "this group has as many live workers as it may",
                "too_many_workers",
            );
        }
        Err(Join::Failed) => {
            return refused(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not record the heartbeat; try again",
                "unavailable",
            );
        }
    };
    if body.leave {
        // By what the cache says this worker holds, not by any tracker's
        // memory: the worker may have heartbeated elsewhere.
        release_all(tracker, groups, &sources, &me).await;
        let mut view = g.view.lock().expect("lease view lock");
        view.held.remove(&me);
        view.refresh(members.into_keys().collect(), &sources);
        drop(view);
        g.bump();
        return Json(json!({ "leases": [] })).into_response();
    }
    let assigned: Vec<String> = {
        let mut view = g.view.lock().expect("lease view lock");
        let moved = view.refresh(members.into_keys().collect(), &sources);
        let assigned = view.assigned(&sources, &me).into_iter().cloned().collect();
        drop(view);
        if moved {
            g.bump();
        }
        assigned
    };
    let leaving: Vec<String> = body
        .held
        .into_iter()
        .filter(|s| sources.contains(s) && !assigned.contains(s))
        .collect();
    let who = me.as_str();
    let mut got: Vec<(String, u64)> = futures::stream::iter(assigned)
        .map(|s| async move {
            let epoch = take_or_renew(tracker, &s, who, now).await;
            epoch.map(|e| (s, e))
        })
        .buffer_unordered(IN_FLIGHT)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    got.sort();
    // A source whose share moved is not renewed, but while its lease is live
    // the worker still holds it: it is told how long, rather than dropped.
    let mut winding: Vec<(String, u64, u64)> = futures::stream::iter(leaving)
        .map(|s| async move {
            let (l, _) = tracker.source_lease(&s).await.ok()??;
            l.held_by(who, now)
                .then(|| (s, l.epoch, l.expires_ms.saturating_sub(now)))
        })
        .buffer_unordered(IN_FLIGHT)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    winding.sort();
    {
        let mut view = g.view.lock().expect("lease view lock");
        if view.held.get(&me) != Some(&got) {
            view.held.insert(me.clone(), got.clone());
            drop(view);
            g.bump();
        }
    }
    let leases: Vec<Value> = got
        .iter()
        .map(|(s, e)| json!({ "source": s, "epoch": e }))
        .chain(
            winding
                .iter()
                .map(|(s, e, left)| json!({ "source": s, "epoch": e, "until_ms": left })),
        )
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
