//! The team table. Each team is a whole tracker of its own: its fold, writer
//! lease, Felix connections and tokens. Nothing in one team's tracker reaches
//! another's, so a lost lease, a log of a newer format or a slow Felix stays in
//! the team it happened to. See "One tracker, many teams" in docs/design.md.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use kbl_core::names::Key;
use kbl_core::org::TeamName;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::JoinSet;

use crate::config::{Caller, Tokens};
use crate::felix::Broker;
use crate::org::Org;
use crate::tracker::Tracker;

/// How long a team that is not serving waits between tries to start it.
pub(crate) const RETRY_EVERY: Duration = Duration::from_secs(5);

/// Starts one team's tracker: connects to its namespace, takes its lease and
/// folds its log, as a starting tracker does.
pub(crate) type Starter<B> =
    Arc<dyn Fn(TeamName) -> Pin<Box<dyn Future<Output = Result<Tracker<B>>> + Send>> + Send + Sync>;

/// What a tracker with teams on has beyond its team table.
pub(crate) struct OrgSide<B> {
    pub(crate) org: Arc<Org<B>>,
    pub(crate) start: Starter<B>,
    /// The token file's people, who are org admins and are in every team.
    pub(crate) admins: Tokens,
    pub(crate) inflight: usize,
}

/// Calls in flight per team when `KBL_TEAM_INFLIGHT` is not set.
pub(crate) const DEFAULT_INFLIGHT: usize = 64;

pub(crate) struct Team<B> {
    name: TeamName,
    /// None while the team is not serving: its lease is lost, or it could not
    /// start. Its calls answer 503 until it is back.
    tracker: RwLock<Option<Tracker<B>>>,
    tokens: Tokens,
    inflight: Arc<Semaphore>,
    /// `work_next` waits for work while it holds a slot, so it has a pool of its
    /// own: agents waiting for work cannot take the slots a holder needs to
    /// extend or finish.
    waiting: Arc<Semaphore>,
}

impl<B: Broker> Team<B> {
    pub(crate) fn new(
        name: TeamName,
        tracker: Tracker<B>,
        tokens: Tokens,
        inflight: usize,
    ) -> Self {
        Self::with(name, Some(tracker), tokens, inflight)
    }

    pub(crate) fn unavailable(name: TeamName, tokens: Tokens, inflight: usize) -> Self {
        Self::with(name, None, tokens, inflight)
    }

    fn with(name: TeamName, tracker: Option<Tracker<B>>, tokens: Tokens, inflight: usize) -> Self {
        Self {
            name,
            tracker: RwLock::new(tracker),
            tokens,
            inflight: Arc::new(Semaphore::new(inflight.max(1))),
            waiting: Arc::new(Semaphore::new(inflight.max(1))),
        }
    }
}

impl<B> Team<B> {
    pub(crate) fn name(&self) -> &TeamName {
        &self.name
    }

    /// The team's tracker, or none while it is not serving.
    pub(crate) fn tracker(&self) -> Option<Tracker<B>> {
        self.tracker.read().expect("team lock").clone()
    }

    fn set_tracker(&self, tracker: Option<Tracker<B>>) {
        *self.tracker.write().expect("team lock") = tracker;
    }

    /// A slot for one call, or none when the team is at its cap. It does not
    /// wait: a team at its cap answers busy at once rather than queueing work
    /// that holds a connection open.
    pub(crate) fn admit(&self, waits: bool) -> Option<OwnedSemaphorePermit> {
        let pool = if waits { &self.waiting } else { &self.inflight };
        pool.clone().try_acquire_owned().ok()
    }
}

pub(crate) struct Teams<B> {
    table: RwLock<BTreeMap<TeamName, Arc<Team<B>>>>,
    org: Option<OrgSide<B>>,
    /// One team start at a time, so two creates of one team start it once.
    starting: Mutex<()>,
    added: mpsc::UnboundedSender<Arc<Team<B>>>,
    added_rx: Mutex<Option<mpsc::UnboundedReceiver<Arc<Team<B>>>>>,
}

impl<B: Broker> Teams<B> {
    /// Refuses two teams of one name, and a token that would name two callers:
    /// an agent or ingest token is bound to one team, and a person token in
    /// several teams must be the same person in each.
    pub(crate) fn new(teams: Vec<Team<B>>) -> Result<Self> {
        Self::with_org(teams, None)
    }

    /// A table whose teams come from `org.events`. A team that loses its lease
    /// or cannot start is retried in the background instead of stopping the
    /// process, and teams can be added while it runs.
    pub(crate) fn with_org(teams: Vec<Team<B>>, org: Option<OrgSide<B>>) -> Result<Self> {
        let mut table = BTreeMap::new();
        let mut seen: HashMap<&str, (&TeamName, Caller)> = HashMap::new();
        for team in &teams {
            for (token, caller) in team.tokens.entries() {
                if let Some((other, before)) = seen.insert(token, (&team.name, caller.clone())) {
                    let same_person = matches!(
                        (&before, &caller),
                        (Caller::Person(a), Caller::Person(b)) if a == b
                    );
                    anyhow::ensure!(
                        same_person,
                        "a token is listed in teams {other} and {}, and is not one person in both",
                        team.name
                    );
                }
            }
        }
        for team in teams {
            let name = team.name.clone();
            anyhow::ensure!(
                table.insert(name.clone(), Arc::new(team)).is_none(),
                "team {name} is listed twice"
            );
        }
        let (added, added_rx) = mpsc::unbounded_channel();
        Ok(Self {
            table: RwLock::new(table),
            org,
            starting: Mutex::new(()),
            added,
            added_rx: Mutex::new(Some(added_rx)),
        })
    }

    fn all(&self) -> Vec<Arc<Team<B>>> {
        self.table
            .read()
            .expect("teams lock")
            .values()
            .cloned()
            .collect()
    }

    pub(crate) fn find(&self, name: &TeamName) -> Option<Arc<Team<B>>> {
        self.table.read().expect("teams lock").get(name).cloned()
    }

    pub(crate) fn org(&self) -> Option<&OrgSide<B>> {
        self.org.as_ref()
    }

    /// The org admin a token names: a person of the token file.
    pub(crate) fn org_admin(&self, token: &str) -> Option<Key> {
        match self.org.as_ref()?.admins.who(token)? {
            Caller::Person(p) => Some(p),
            Caller::Agent(_) | Caller::Ingest(_) => None,
        }
    }

    /// Serves `name` if it is not served yet: starts its tracker, or adds it
    /// as not serving and leaves it to the background retry. Whether it serves.
    pub(crate) async fn ensure(&self, name: &TeamName) -> bool {
        let Some(org) = self.org.as_ref() else {
            return self.find(name).is_some_and(|t| t.tracker().is_some());
        };
        let _one = self.starting.lock().await;
        if let Some(team) = self.find(name) {
            return team.tracker().is_some();
        }
        let tracker = match (org.start)(name.clone()).await {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("kbl-tracker: team {name} did not start: {e:#}");
                None
            }
        };
        let serving = tracker.is_some();
        let team = Arc::new(Team::with(
            name.clone(),
            tracker,
            org.admins.people_only(),
            org.inflight,
        ));
        self.table
            .write()
            .expect("teams lock")
            .insert(name.clone(), team.clone());
        let _ = self.added.send(team);
        serving
    }

    /// Every team the token belongs to, with who it is there. Each team's
    /// tokens are compared in full, so the time taken does not say where a
    /// token matched.
    pub(crate) fn who(&self, token: &str) -> Vec<(Arc<Team<B>>, Caller)> {
        let mut found = Vec::new();
        for team in self.all() {
            if let Some(caller) = team.tokens.who(token) {
                found.push((team.clone(), caller));
            }
        }
        found
    }

    #[cfg(test)]
    pub(crate) fn get(&self, name: &str) -> Option<Arc<Team<B>>> {
        self.all().into_iter().find(|t| t.name.as_str() == name)
    }

    /// Runs each team's lease renewal and sweep. Without teams on, a team
    /// that loses its lease for good stops here and answers 503 from then on,
    /// and this returns when no team holds its lease. With teams on, such a
    /// team is started again in the background, and this returns only when
    /// the org lease has been lost for its grace period. Either way it returns
    /// at once when a team's maintenance panicked: that team would go on
    /// writing with no sweep.
    pub(crate) async fn maintain(&self) {
        let Some(mut added) = self.added_rx.lock().await.take() else {
            return;
        };
        let mut running = JoinSet::new();
        for team in self.all() {
            running.spawn(self.keep(team));
        }
        let org = self.org.as_ref().map(|o| o.org.clone());
        let org_lost = async move {
            match org {
                Some(org) => org.maintain().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(org_lost);
        loop {
            tokio::select! {
                Some(team) = added.recv() => {
                    running.spawn(self.keep(team));
                }
                Some(done) = running.join_next(), if !running.is_empty() => match done {
                    Ok(name) => {
                        eprintln!("kbl-tracker: team {name} lost its writer lease");
                        if running.is_empty() && self.org.is_none() {
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("kbl-tracker: a team's maintenance failed: {e}");
                        return;
                    }
                },
                () = &mut org_lost => {
                    eprintln!("kbl-tracker: the org writer lease was lost for too long");
                    return;
                }
            }
        }
    }

    /// One team's maintenance. With teams on it never ends: a lost lease, or
    /// a start that failed, is tried again until the team serves.
    fn keep(&self, team: Arc<Team<B>>) -> impl Future<Output = TeamName> + Send + 'static {
        let start = self.org.as_ref().map(|o| o.start.clone());
        async move {
            loop {
                if let Some(tracker) = team.tracker() {
                    tracker.maintain().await;
                }
                let Some(start) = start.as_ref() else {
                    return team.name.clone();
                };
                if team.tracker().is_some() {
                    eprintln!(
                        "kbl-tracker: team {} lost its writer lease; taking it back",
                        team.name
                    );
                    team.set_tracker(None);
                }
                // A fresh tracker folds the log again, as a restart does: the
                // old fold may miss what another writer added meanwhile.
                loop {
                    tokio::time::sleep(RETRY_EVERY).await;
                    match start(team.name.clone()).await {
                        Ok(tracker) => {
                            team.set_tracker(Some(tracker));
                            eprintln!("kbl-tracker: team {} is serving again", team.name);
                            break;
                        }
                        Err(e) => eprintln!("kbl-tracker: team {} did not start: {e:#}", team.name),
                    }
                }
            }
        }
    }
}
