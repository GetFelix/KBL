//! Supervise: this agent starts and tops up a pool of worker agents from the
//! launch profiles in `KBL_SUPERVISE_CONFIG`. Nothing here calls a model; a
//! worker is a configured command or hook, picked by a fixed table. See
//! docs/design/supervise.md.

mod config;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use kbl_core::names::AgentName;
use serde_json::{Value, json};
use tokio::process::Child;
use tokio::sync::Mutex;

pub(crate) use config::Config;
use config::{COMMAND_VARS, Fields, Kind, PATH_VARS, Profile, check_model};
use render::Vars;

use crate::work::{CallError, TrackerApi};

/// The most workers one pool asks for, the fold's own cap. The tracker's
/// caps are usually lower and are the ones that bind.
pub(crate) const MAX_POOL: usize = 64;
/// Workers in a row that fail or exit too soon before the top-up pauses.
const PAUSE_AFTER: u32 = 3;
/// How many ready items one look at the queue reads.
const PEEK: u64 = 50;
/// How long a hook may take to answer.
const HOOK_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a killed worker gets to exit before it is left to the OS.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// What every worker's MCP config carries besides its own name and token:
/// the run, the addresses, and Felix credentials by reference.
pub(crate) struct Base {
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) tracker_url: String,
}

/// Reads the supervisor's environment, for `pass_env` and hook bearers.
pub(crate) type EnvReader = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub(crate) struct Supervisor<T>(Arc<Inner<T>>);

impl<T> Clone for Supervisor<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

struct Inner<T> {
    api: T,
    me: AgentName,
    config: Config,
    base: Base,
    kbl_mcp: PathBuf,
    state_dir: PathBuf,
    hook: reqwest::Client,
    /// Reads the supervisor's environment, for `pass_env` and hook bearers.
    env: EnvReader,
    pool: Mutex<Pool>,
}

#[derive(Default)]
struct Pool {
    target: usize,
    filter: Value,
    profile: Option<String>,
    model: Option<String>,
    armed: bool,
    paused: Option<String>,
    quick_failures: u32,
    workers: BTreeMap<String, Running>,
    finished: Vec<Value>,
    failed: Vec<Value>,
    /// Every worker name this supervisor has had, so none is asked for twice.
    used: BTreeSet<String>,
    /// Whether workers an earlier process left have been stopped.
    adopted: bool,
    last_error: Option<String>,
}

struct Running {
    kind: Kind,
    profile: String,
    vendor: String,
    model: Option<String>,
    started: Instant,
    child: Option<Child>,
    /// The MCP config holding the worker's token, removed when it stops.
    config_file: Option<PathBuf>,
    /// The tracker did not take the stop yet; it is sent again each round.
    stop_pending: bool,
}

/// Why a launch did not happen.
#[derive(Debug)]
enum Launch {
    /// The tracker's worker cap: no more this round.
    Cap,
    Failed(String),
}

impl<T: TrackerApi> Supervisor<T> {
    pub(crate) fn new(
        api: T,
        me: AgentName,
        config: Config,
        base: Base,
        env: EnvReader,
    ) -> Result<Self> {
        let kbl_mcp = match &config.kbl_mcp {
            Some(p) => render::absolute(p),
            None => std::env::current_exe().context("find this kbl-mcp binary")?,
        };
        let state_dir = match &config.state_dir {
            Some(d) => render::absolute(d),
            None => env("HOME")
                .map(|h| PathBuf::from(h).join(".local/state/kbl/supervise"))
                .context("set state_dir in the supervise config, or HOME")?,
        }
        .join(me.as_str());
        render::private_dir(&state_dir)?;
        let hook = reqwest::Client::builder()
            .timeout(HOOK_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self(Arc::new(Inner {
            api,
            me,
            config,
            base,
            kbl_mcp,
            state_dir,
            hook,
            env,
            pool: Mutex::new(Pool::default()),
        })))
    }

    /// Runs the top-up every `top_up_seconds` until the process ends.
    pub(crate) fn spawn_top_up(&self) {
        let me = self.clone();
        let every = self.0.config.top_up_every();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                me.round().await;
            }
        });
    }

    /// One top-up round: reap, then fill the pool.
    pub(crate) async fn round(&self) {
        let mut pool = self.0.pool.lock().await;
        if !pool.armed && pool.workers.is_empty() {
            return;
        }
        let result = self.reap(&mut pool).await;
        let result = match result {
            Ok(()) => self.top_up(&mut pool).await.map(|_| ()),
            Err(e) => Err(e),
        };
        pool.last_error = result.err().map(|e| format!("{e:#}"));
    }

    /// `supervise_start`: the pool becomes `count` workers on `filter`.
    pub(crate) async fn start(
        &self,
        count: usize,
        filter: Value,
        profile: Option<String>,
        model: Option<String>,
    ) -> Result<Value> {
        if !(1..=MAX_POOL).contains(&count) {
            bail!("count must be 1 to {MAX_POOL}");
        }
        if let Some(p) = &profile {
            self.0.config.profile(p)?;
        }
        if let Some(m) = &model {
            check_model(m)?;
        }
        let mut pool = self.0.pool.lock().await;
        self.reap(&mut pool).await?;
        pool.target = count;
        pool.filter = filter;
        pool.profile = profile;
        pool.model = model;
        pool.armed = true;
        pool.paused = None;
        pool.quick_failures = 0;
        let started = self.top_up(&mut pool).await?;
        let live = live(&pool);
        Ok(json!({
            "started": started,
            "live": live,
            "target": count,
            "note": if started.is_empty() && live < count {
                "Nothing was started: no ready work matches the filter, or the tracker's worker cap is reached. The top-up keeps looking."
            } else {
                "The top-up keeps the pool full while ready work matches the filter."
            },
        }))
    }

    /// `supervise_stop`: one worker, lowering the pool by one, or all of them.
    pub(crate) async fn stop(&self, worker: Option<String>, all: bool) -> Result<Value> {
        let mut pool = self.0.pool.lock().await;
        let names: Vec<String> = match (worker, all) {
            (None, true) => {
                pool.armed = false;
                pool.target = 0;
                self.adopt(&mut pool).await?;
                pool.workers.keys().cloned().collect()
            }
            (Some(w), false) => {
                if !pool.workers.contains_key(&w) {
                    bail!("{w} is not one of this supervisor's live workers");
                }
                pool.target = pool.target.saturating_sub(1);
                vec![w]
            }
            _ => bail!("give either worker or all: true"),
        };
        let mut stopped = Vec::new();
        for name in names {
            self.stop_worker(&mut pool, &name).await;
            if !pool.workers.contains_key(&name) {
                stopped.push(name);
            }
        }
        let pending: Vec<&String> = pool.workers.keys().collect();
        Ok(json!({ "stopped": stopped, "stop_pending": pending, "target": pool.target }))
    }

    /// `supervise_status`: the pool, what each worker holds, who finished or
    /// failed since the last call, and the queue's shape.
    pub(crate) async fn status(&self) -> Result<Value> {
        let mut pool = self.0.pool.lock().await;
        self.reap(&mut pool).await?;
        let listed = self.0.api.get("/v1/work/workers").await.map_err(text)?;
        let holds: BTreeMap<&str, &Value> = listed["workers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| Some((w["worker"].as_str()?, &w["holds"])))
            .collect();
        let workers: Vec<Value> = pool
            .workers
            .iter()
            .map(|(name, r)| {
                json!({
                    "worker": name,
                    "kind": if r.kind == Kind::Hook { "hook" } else { "command" },
                    "vendor": r.vendor,
                    "profile": r.profile,
                    "model": r.model,
                    "running_s": r.started.elapsed().as_secs(),
                    "holds": holds.get(name.as_str()).copied().unwrap_or(&Value::Null),
                    "stop_pending": r.stop_pending,
                })
            })
            .collect();
        let queue = if pool.armed {
            let peeked = self.peek(&pool.filter).await?;
            shape(&peeked)
        } else {
            Value::Null
        };
        let finished = std::mem::take(&mut pool.finished);
        let failed = std::mem::take(&mut pool.failed);
        Ok(json!({
            "target": pool.target,
            "top_up": if !pool.armed { "off" } else if pool.paused.is_some() { "paused" } else { "on" },
            "paused_because": pool.paused,
            "last_error": pool.last_error,
            "filter": pool.filter,
            "workers": workers,
            "finished": finished,
            "failed": failed,
            "queue": queue,
        }))
    }

    /// Stops the live workers the tracker lists for this supervisor that this
    /// process did not start: an earlier process left them with nobody to
    /// stop them.
    async fn adopt(&self, pool: &mut Pool) -> Result<()> {
        if pool.adopted {
            return Ok(());
        }
        let listed = self.0.api.get("/v1/work/workers").await.map_err(text)?;
        for w in listed["workers"].as_array().into_iter().flatten() {
            let Some(name) = w["worker"].as_str() else {
                continue;
            };
            pool.used.insert(name.to_owned());
            if w["stopped_ms"].is_null() && !pool.workers.contains_key(name) {
                self.0
                    .api
                    .call("/v1/work/workers/stop", json!({ "worker": name }))
                    .await
                    .map_err(text)?;
            }
        }
        pool.adopted = true;
        Ok(())
    }

    /// Notes the workers whose process ended and stops them on the tracker,
    /// and sends again any stop the tracker did not take.
    async fn reap(&self, pool: &mut Pool) -> Result<()> {
        self.adopt(pool).await?;
        let min_run = Duration::from_secs(self.0.config.min_run_seconds);
        let mut ended = Vec::new();
        for (name, r) in pool.workers.iter_mut() {
            let Some(child) = r.child.as_mut() else {
                continue;
            };
            if let Ok(Some(status)) = child.try_wait() {
                let ran = r.started.elapsed();
                ended.push((name.clone(), status.code(), ran));
                r.child = None;
            }
        }
        for (name, code, ran) in ended {
            let r = &pool.workers[&name];
            let outcome = json!({
                "worker": name,
                "profile": r.profile,
                "model": r.model,
                "exit": code,
                "ran_s": ran.as_secs(),
            });
            if code == Some(0) {
                pool.finished.push(outcome);
            } else {
                pool.failed.push(outcome);
            }
            if code != Some(0) || ran < min_run {
                pool.quick_failures += 1;
                if pool.quick_failures >= PAUSE_AFTER && pool.paused.is_none() {
                    pool.paused = Some(format!(
                        "{PAUSE_AFTER} workers in a row failed or exited within {} s; supervise_start resumes",
                        min_run.as_secs()
                    ));
                }
            } else {
                pool.quick_failures = 0;
            }
            self.stop_worker(pool, &name).await;
        }
        let pending: Vec<String> = pool
            .workers
            .iter()
            .filter(|(_, r)| r.stop_pending)
            .map(|(n, _)| n.clone())
            .collect();
        for name in pending {
            self.stop_worker(pool, &name).await;
        }
        Ok(())
    }

    /// Starts workers until the pool is full or the ready work runs out, one
    /// per ready item that no idle worker is already waiting for.
    async fn top_up(&self, pool: &mut Pool) -> Result<Vec<String>> {
        if !pool.armed || pool.paused.is_some() {
            return Ok(Vec::new());
        }
        let live = live(pool);
        if live >= pool.target {
            return Ok(Vec::new());
        }
        let peeked = self.peek(&pool.filter).await?;
        let ready = peeked["next"].as_array().cloned().unwrap_or_default();
        let idle = self.idle(pool).await?;
        let room = (pool.target - live).min(ready.len().saturating_sub(idle));
        let mut started = Vec::new();
        for item in ready.iter().skip(idle).take(room) {
            match self.launch(pool, &Fields::of_peeked(item)).await {
                Ok(name) => started.push(name),
                Err(Launch::Cap) => break,
                Err(Launch::Failed(why)) => {
                    pool.last_error = Some(why);
                    pool.quick_failures += 1;
                    if pool.quick_failures >= PAUSE_AFTER {
                        pool.paused = Some(format!(
                            "{PAUSE_AFTER} launches in a row failed; supervise_start resumes"
                        ));
                        break;
                    }
                }
            }
        }
        Ok(started)
    }

    /// Live workers that hold nothing yet: each is about to take a ready item.
    async fn idle(&self, pool: &Pool) -> Result<usize> {
        let listed = self.0.api.get("/v1/work/workers").await.map_err(text)?;
        let holding: BTreeSet<&str> = listed["workers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|w| !w["holds"].is_null())
            .filter_map(|w| w["worker"].as_str())
            .collect();
        Ok(pool
            .workers
            .iter()
            .filter(|(n, r)| !r.stop_pending && !holding.contains(n.as_str()))
            .count())
    }

    async fn peek(&self, filter: &Value) -> Result<Value> {
        let mut body = json!({ "limit": PEEK });
        if let (Some(into), Some(from)) = (body.as_object_mut(), filter.as_object()) {
            into.extend(from.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        self.0.api.call("/v1/work/peek", body).await.map_err(text)
    }

    async fn launch(&self, pool: &mut Pool, item: &Fields) -> Result<String, Launch> {
        let cfg = &self.0.config;
        let (profile_name, fit_model) = match &pool.profile {
            Some(p) => (p.clone(), None),
            None => match cfg.fit(Some(item)) {
                Some((p, m)) => (p.to_owned(), m.map(str::to_owned)),
                None => {
                    return Err(Launch::Failed(
                        "no fit row matches and there is no default_profile".into(),
                    ));
                }
            },
        };
        let profile = cfg.profile(&profile_name).map_err(fail)?;
        let model = pool
            .model
            .clone()
            .or(fit_model)
            .or_else(|| profile.model.clone());
        if profile.kind == Kind::Command && profile.needs_model() && model.is_none() {
            return Err(Launch::Failed(format!(
                "profile {profile_name} fills {{model}} and this launch has none"
            )));
        }
        let worker = self.next_name(pool);
        let body = json!({
            "worker": worker,
            "vendor": profile.vendor,
            "profile": profile_name,
            "model": model,
            "exchange": profile.kind == Kind::Hook,
        });
        let reply = match self.0.api.call("/v1/work/workers", body).await {
            Ok(r) => r,
            Err(CallError::Refused(m)) if m.contains("worker_cap") => return Err(Launch::Cap),
            Err(e) => return Err(Launch::Failed(text(e).to_string())),
        };
        let mut running = Running {
            kind: profile.kind,
            profile: profile_name,
            vendor: profile.vendor.clone(),
            model,
            started: Instant::now(),
            child: None,
            config_file: None,
            stop_pending: false,
        };
        let went = match profile.kind {
            Kind::Command => {
                let token = reply["token"].as_str().unwrap_or_default();
                self.spawn(profile, &worker, token, &pool.filter, &mut running)
            }
            Kind::Hook => {
                let code = reply["code"].as_str().unwrap_or_default();
                self.call_hook(profile, &worker, code, &running, &pool.filter)
                    .await
            }
        };
        pool.workers.insert(worker.clone(), running);
        if let Err(e) = went {
            // The token is live on the tracker: stop it before anything else.
            self.stop_worker(pool, &worker).await;
            return Err(Launch::Failed(format!("{worker}: {e:#}")));
        }
        Ok(worker)
    }

    fn next_name(&self, pool: &mut Pool) -> String {
        let name = (1..)
            .map(|n| format!("{}-w{n}", self.0.me.as_str()))
            .find(|n| !pool.used.contains(n))
            .expect("an unused name");
        pool.used.insert(name.clone());
        name
    }

    /// Writes the worker's MCP config and starts its command, with an empty
    /// environment plus what the profile passes, so no secret of the
    /// supervisor's reaches the worker.
    fn spawn(
        &self,
        profile: &Profile,
        worker: &str,
        token: &str,
        filter: &Value,
        running: &mut Running,
    ) -> Result<()> {
        if token.is_empty() {
            bail!("the tracker answered no token");
        }
        let own = self.0.state_dir.join(worker);
        render::private_dir(&own)?;
        let workdir = match &profile.workdir {
            Some(w) => {
                let vars = Vars {
                    model: None,
                    worker,
                    mcp_config_path: None,
                    workdir: None,
                };
                let dir = render::absolute(Path::new(&render::fill(w, &["worker"], &vars)?));
                render::private_dir(&dir)?;
                dir
            }
            None => own.clone(),
        };
        let workdir_s = workdir.to_str().context("the workdir is not UTF-8")?;
        let path_vars = Vars {
            model: None,
            worker,
            mcp_config_path: None,
            workdir: Some(workdir_s),
        };
        let config_file = match &profile.mcp_config_file {
            Some(f) => render::absolute(Path::new(&render::fill(f, &PATH_VARS, &path_vars)?)),
            None => own.join(match profile.mcp_config {
                config::Format::Json => "mcp.json",
                config::Format::Toml => "mcp.toml",
            }),
        };
        let mut env = self.0.base.env.clone();
        env.insert("KBL_AGENT".into(), worker.into());
        env.insert("KBL_TRACKER_TOKEN".into(), token.into());
        env.insert("KBL_WORK_FILTER".into(), filter.to_string());
        let text = render::mcp_config(profile.mcp_config, &self.0.kbl_mcp, &env)?;
        render::write_private(&config_file, &text)?;
        running.config_file = Some(config_file.clone());
        let config_s = config_file
            .to_str()
            .context("the MCP config path is not UTF-8")?;
        let vars = Vars {
            model: running.model.as_deref(),
            worker,
            mcp_config_path: Some(config_s),
            workdir: Some(workdir_s),
        };
        let (program, args) = profile.command.split_first().expect("checked at load");
        let args = args
            .iter()
            .map(|a| render::fill(a, &COMMAND_VARS, &vars))
            .collect::<Result<Vec<_>>>()?;
        let log = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(own.join("out.log"))
            .context("open the worker's log")?;
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&args)
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true);
        let passed = profile
            .pass_env
            .iter()
            .filter_map(|name| Some((name, (self.0.env)(name)?)));
        cmd.env_clear().envs(passed);
        for (k, v) in &profile.set_env {
            cmd.env(k, render::fill(v, &PATH_VARS, &path_vars)?);
        }
        running.child = Some(cmd.spawn().with_context(|| format!("start {program}"))?);
        Ok(())
    }

    /// Hands a hosted agent its worker name and a one-time code; the token
    /// itself stays with the tracker until the agent trades the code.
    async fn call_hook(
        &self,
        profile: &Profile,
        worker: &str,
        code: &str,
        running: &Running,
        filter: &Value,
    ) -> Result<()> {
        if code.is_empty() {
            bail!("the tracker answered no code");
        }
        let url = profile.url.as_deref().expect("checked at load");
        let mut req = self.0.hook.post(url).json(&json!({
            "worker": worker,
            "supervisor": self.0.me.as_str(),
            "code": code,
            "tracker_url": self.0.base.tracker_url,
            "vendor": running.vendor,
            "profile": running.profile,
            "model": running.model,
            "filter": filter,
        }));
        if let Some(name) = &profile.hook_token_env {
            let bearer = (self.0.env)(name).with_context(|| format!("{name} is not set"))?;
            req = req.bearer_auth(bearer);
        }
        let status = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("the hook did not answer: {}", e.without_url()))?
            .status();
        if !status.is_success() {
            bail!("the hook answered {status}");
        }
        Ok(())
    }

    /// Stops a worker: on the tracker first, so its token dies before its
    /// process does, then the process and its config file.
    async fn stop_worker(&self, pool: &mut Pool, name: &str) {
        let Some(r) = pool.workers.get_mut(name) else {
            return;
        };
        let stopped = match self
            .0
            .api
            .call("/v1/work/workers/stop", json!({ "worker": name }))
            .await
        {
            // Refused means the tracker has it stopped already, or never had it.
            Ok(_) | Err(CallError::Refused(_)) => true,
            Err(_) => false,
        };
        if let Some(mut child) = r.child.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(KILL_WAIT, child.wait()).await;
        }
        if let Some(f) = r.config_file.take() {
            let _ = std::fs::remove_file(f);
        }
        if stopped {
            pool.workers.remove(name);
        } else {
            r.stop_pending = true;
        }
    }
}

fn live(pool: &Pool) -> usize {
    pool.workers.values().filter(|r| !r.stop_pending).count()
}

/// Ready counts by kind and by tier, from a peek.
fn shape(peeked: &Value) -> Value {
    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    let mut tiers: BTreeMap<String, u64> = BTreeMap::new();
    let next = peeked["next"].as_array().cloned().unwrap_or_default();
    for it in &next {
        *kinds
            .entry(it["work"].as_str().unwrap_or("?").to_owned())
            .or_default() += 1;
        *tiers.entry(it["tier"].to_string()).or_default() += 1;
    }
    json!({ "ready": next.len(), "ready_counted_up_to": PEEK, "by_kind": kinds, "by_tier": tiers })
}

fn text(e: CallError) -> anyhow::Error {
    anyhow::Error::msg(crate::work::message(e))
}

fn fail(e: anyhow::Error) -> Launch {
    Launch::Failed(format!("{e:#}"))
}

#[cfg(test)]
mod tests;
