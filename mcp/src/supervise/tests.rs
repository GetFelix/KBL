//! The pool against a scripted tracker and a shell script standing in for a
//! vendor's agent, so launch, stop, top-up and its brakes run for real.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

#[derive(Clone, Default)]
struct Fake(Arc<StdMutex<Script>>);

#[derive(Default)]
struct Script {
    ready: Vec<Value>,
    cap: Option<usize>,
    started: Vec<Value>,
    stopped: Vec<String>,
    holds: BTreeMap<String, Value>,
    /// Workers an earlier process left live.
    left: Vec<String>,
}

impl TrackerApi for Fake {
    async fn call(&self, path: &str, body: Value) -> Result<Value, CallError> {
        let mut s = self.0.lock().unwrap();
        match path {
            "/v1/work/peek" => Ok(json!({ "next": s.ready })),
            "/v1/work/workers" => {
                let live = s.started.len().saturating_sub(s.stopped.len());
                if s.cap.is_some_and(|c| live >= c) {
                    return Err(CallError::Refused("refused: worker_cap".into()));
                }
                let name = body["worker"].as_str().unwrap().to_owned();
                s.started.push(body.clone());
                if body["exchange"] == json!(true) {
                    Ok(json!({ "worker": name, "code": format!("kblc_{name}") }))
                } else {
                    Ok(json!({ "worker": name, "token": format!("kblw_{name}") }))
                }
            }
            "/v1/work/workers/stop" => {
                s.stopped.push(body["worker"].as_str().unwrap().to_owned());
                Ok(json!({ "kind": "worker_stopped" }))
            }
            _ => Err(CallError::Down(format!("no route {path}"))),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, CallError> {
        assert_eq!(path, "/v1/work/workers");
        let s = self.0.lock().unwrap();
        let mut list: Vec<Value> = s
            .started
            .iter()
            .map(|b| {
                let name = b["worker"].as_str().unwrap();
                let stopped = s.stopped.iter().any(|x| x == name);
                json!({
                    "worker": name,
                    "stopped_ms": if stopped { json!(1) } else { Value::Null },
                    "holds": s.holds.get(name).cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        for l in &s.left {
            if !s.stopped.contains(l) {
                list.push(json!({ "worker": l, "stopped_ms": null, "holds": null }));
            }
        }
        Ok(json!({ "workers": list }))
    }
}

static DIRS: AtomicU32 = AtomicU32::new(0);

fn scratch() -> PathBuf {
    let n = DIRS.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("kbl-sup-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The stand-in agent: records its argv, environment and config file, then
/// exits with `EXIT` after `SLEEP` seconds.
const AGENT: &str = r#"
printf '%s\n' "$@" > "$PWD/argv"
env > "$PWD/env"
cp "$1" "$PWD/config.copy"
ls -l "$1" | cut -c1-10 > "$PWD/mode"
sleep "${SLEEP:-0}"
exit "${EXIT:-0}"
"#;

fn setup(dir: &Path, extra: &str, exit: &str, sleep: &str, fake: &Fake) -> Supervisor<Fake> {
    let agent = dir.join("agent.sh");
    std::fs::write(&agent, AGENT).unwrap();
    let text = format!(
        r#"
state_dir = "{state}"
kbl_mcp = "/opt/kbl/kbl-mcp"
default_profile = "fake"
top_up_seconds = 5
[profiles.fake]
kind = "command"
vendor = "fake"
model = "small"
command = ["/bin/sh", "{agent}", "{{mcp_config_path}}", "--model", "{{model}}", "a b; rm -rf /", "{{worker}}"]
workdir = "{dir}/work/{{worker}}"
pass_env = ["PATH", "VENDOR_KEY"]
set_env = {{ EXIT = "{exit}", SLEEP = "{sleep}" }}
{extra}
"#,
        state = dir.join("state").display(),
        agent = agent.display(),
        dir = dir.display(),
    );
    let config = Config::parse(&text).unwrap();
    let base = Base {
        env: BTreeMap::from([("KBL_RUN".to_owned(), "r1".to_owned())]),
        tracker_url: "http://127.0.0.1:1".into(),
    };
    let env: EnvReader = Box::new(|name| match name {
        "PATH" => std::env::var("PATH").ok(),
        "VENDOR_KEY" => Some("vk".into()),
        "KBL_TRACKER_TOKEN" => Some("SUPERVISOR-SECRET".into()),
        _ => None,
    });
    Supervisor::new(
        fake.clone(),
        AgentName::new("sup").unwrap(),
        config,
        base,
        env,
    )
    .unwrap()
}

fn ready(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| json!({ "item": format!("i{i}"), "work": "fix", "tier": 1 }))
        .collect()
}

async fn until_reaped(sup: &Supervisor<Fake>, want: usize) {
    for _ in 0..200 {
        sup.round().await;
        let pool = sup.0.pool.lock().await;
        if pool.finished.len() + pool.failed.len() >= want {
            return;
        }
        drop(pool);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("workers did not end");
}

#[tokio::test]
async fn a_worker_gets_its_own_token_in_a_private_file_and_none_of_the_supervisors() {
    let dir = scratch();
    let fake = Fake::default();
    fake.0.lock().unwrap().ready = ready(3);
    let sup = setup(&dir, "", "0", "0", &fake);
    let reply = sup
        .start(2, json!({ "repo": "o/r" }), None, None)
        .await
        .unwrap();
    assert_eq!(reply["started"], json!(["sup-w1", "sup-w2"]));
    until_reaped(&sup, 2).await;

    let work = dir.join("work/sup-w1");
    let argv = std::fs::read_to_string(work.join("argv")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    let config_path = dir.join("state/sup/sup-w1/mcp.json");
    assert_eq!(
        argv,
        [
            config_path.to_str().unwrap(),
            "--model",
            "small",
            "a b; rm -rf /",
            "sup-w1"
        ],
        "every element reaches the program as it is, with no shell between"
    );
    assert_eq!(
        std::fs::read_to_string(work.join("mode")).unwrap().trim(),
        "-rw-------"
    );
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(work.join("config.copy")).unwrap()).unwrap();
    let env = &config["mcpServers"]["kbl"]["env"];
    assert_eq!(config["mcpServers"]["kbl"]["command"], "/opt/kbl/kbl-mcp");
    assert_eq!(env["KBL_AGENT"], "sup-w1");
    assert_eq!(env["KBL_TRACKER_TOKEN"], "kblw_sup-w1");
    assert_eq!(env["KBL_RUN"], "r1");
    assert_eq!(env["KBL_WORK_FILTER"], r#"{"repo":"o/r"}"#);

    let proc_env = std::fs::read_to_string(work.join("env")).unwrap();
    assert!(proc_env.contains("VENDOR_KEY=vk"));
    assert!(!proc_env.contains("SUPERVISOR-SECRET"), "{proc_env}");
    assert!(!proc_env.contains("kblw_"), "the token is only in the file");
    assert!(
        !proc_env.lines().any(|l| l.starts_with("KBL_")),
        "{proc_env}"
    );
    // HOME is in the supervisor's environment but not in pass_env.
    assert!(
        !proc_env.lines().any(|l| l.starts_with("HOME=")),
        "{proc_env}"
    );

    // The config file holding the token is gone once the worker stops.
    assert!(!config_path.exists());
    let s = fake.0.lock().unwrap();
    assert_eq!(s.stopped, ["sup-w1", "sup-w2"]);
    assert_eq!(s.started[0]["vendor"], "fake");
    assert_eq!(s.started[0]["profile"], "fake");
    assert_eq!(s.started[0]["model"], "small");
}

#[tokio::test]
async fn the_fit_table_picks_each_launch_and_the_call_overrides_it() {
    let dir = scratch();
    let fake = Fake::default();
    fake.0.lock().unwrap().ready = vec![
        json!({ "item": "a", "work": "fix", "tier": 0 }),
        json!({ "item": "b", "work": "triage", "tier": 2 }),
    ];
    let fit = "[[fit]]\ntiers = [0]\nprofile = \"fake\"\nmodel = \"opus\"\n";
    let sup = setup(&dir, fit, "0", "5", &fake);
    sup.start(2, json!({}), None, None).await.unwrap();
    {
        let s = fake.0.lock().unwrap();
        assert_eq!(s.started[0]["model"], "opus");
        assert_eq!(s.started[1]["model"], "small");
    }
    sup.stop(None, true).await.unwrap();
    sup.start(1, json!({}), None, Some("override".into()))
        .await
        .unwrap();
    assert_eq!(fake.0.lock().unwrap().started[2]["model"], "override");
    sup.stop(None, true).await.unwrap();
}

#[tokio::test]
async fn the_top_up_starts_no_more_workers_than_ready_items_or_the_cap() {
    let dir = scratch();
    let fake = Fake::default();
    fake.0.lock().unwrap().ready = ready(2);
    let sup = setup(&dir, "", "0", "30", &fake);
    let reply = sup.start(5, json!({}), None, None).await.unwrap();
    assert_eq!(reply["started"].as_array().unwrap().len(), 2);
    // Both are idle (no hold yet), so the same two ready items start nobody new.
    sup.round().await;
    assert_eq!(fake.0.lock().unwrap().started.len(), 2);
    // Once they hold their items and more work arrives, the pool grows to the cap.
    {
        let mut s = fake.0.lock().unwrap();
        s.holds.insert("sup-w1".into(), json!({ "item": "i0" }));
        s.holds.insert("sup-w2".into(), json!({ "item": "i1" }));
        s.ready = ready(10);
        s.cap = Some(4);
    }
    sup.round().await;
    let status = sup.status().await.unwrap();
    assert_eq!(status["workers"].as_array().unwrap().len(), 4);
    assert_eq!(status["queue"]["ready"], 10);
    assert_eq!(status["queue"]["by_kind"]["fix"], 10);
    assert_eq!(status["top_up"], "on");
    sup.stop(Some("sup-w3".into()), false).await.unwrap();
    assert_eq!(sup.0.pool.lock().await.target, 4);
    let stopped = sup.stop(None, true).await.unwrap();
    assert_eq!(stopped["stopped"].as_array().unwrap().len(), 3);
    assert_eq!(sup.status().await.unwrap()["top_up"], "off");
}

#[tokio::test]
async fn workers_that_keep_failing_pause_the_top_up() {
    let dir = scratch();
    let fake = Fake::default();
    fake.0.lock().unwrap().ready = ready(50);
    let sup = setup(&dir, "", "1", "0", &fake);
    sup.start(1, json!({}), None, None).await.unwrap();
    until_reaped(&sup, 3).await;
    for _ in 0..5 {
        sup.round().await;
    }
    let status = sup.status().await.unwrap();
    assert_eq!(status["top_up"], "paused", "{status}");
    assert_eq!(status["failed"].as_array().unwrap().len(), 3);
    assert_eq!(
        fake.0.lock().unwrap().started.len(),
        3,
        "no launch after the pause"
    );
    // A second look shows no failures it already showed.
    assert_eq!(sup.status().await.unwrap()["failed"], json!([]));
}

#[tokio::test]
async fn workers_an_earlier_process_left_are_stopped_and_their_names_not_reused() {
    let dir = scratch();
    let fake = Fake::default();
    {
        let mut s = fake.0.lock().unwrap();
        s.left = vec!["sup-w1".into()];
        s.ready = ready(1);
    }
    let sup = setup(&dir, "", "0", "5", &fake);
    let reply = sup.start(1, json!({}), None, None).await.unwrap();
    assert_eq!(reply["started"], json!(["sup-w2"]));
    assert_eq!(fake.0.lock().unwrap().stopped, ["sup-w1"]);
    sup.stop(None, true).await.unwrap();
}

#[tokio::test]
async fn a_hook_gets_a_code_never_a_token_and_a_failed_hook_stops_its_worker() {
    let dir = scratch();
    let fake = Fake::default();
    fake.0.lock().unwrap().ready = ready(1);
    // Nothing listens on port 9, so the hook fails.
    let hook = "[profiles.cloud]\nkind = \"hook\"\nvendor = \"cloud\"\nurl = \"http://127.0.0.1:9/start\"\n";
    let sup = setup(&dir, hook, "0", "0", &fake);
    let reply = sup
        .start(1, json!({}), Some("cloud".into()), None)
        .await
        .unwrap();
    assert_eq!(reply["started"], json!([]));
    let s = fake.0.lock().unwrap();
    assert_eq!(s.started[0]["exchange"], true);
    assert_eq!(s.stopped, ["sup-w1"]);
}

#[tokio::test]
async fn the_state_dir_is_private() {
    let dir = scratch();
    let sup = setup(&dir, "", "0", "0", &Fake::default());
    let mode = std::fs::metadata(&sup.0.state_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
}
