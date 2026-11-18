use std::sync::Mutex;

use super::*;
use crate::budget::{Budget, PoolShare, Vendor};

/// A tracker that keeps one pool's backoff, as the real one does: the later
/// time wins and answers are relative.
#[derive(Default)]
struct Pool {
    left: Mutex<Duration>,
    puts: Mutex<Vec<Duration>>,
    down: Mutex<bool>,
}

impl PoolApi for Pool {
    async fn backoff(&self, _pool: &str, wait: Option<Duration>) -> Result<Duration> {
        if *self.down.lock().unwrap() {
            bail!("the tracker is down");
        }
        let mut left = self.left.lock().unwrap();
        if let Some(w) = wait {
            self.puts.lock().unwrap().push(w);
            *left = (*left).max(w.min(Duration::from_secs(3600)));
        }
        Ok(*left)
    }
}

fn worker() -> Arc<Budget> {
    let b = Budget::new(Vendor::Github, None, 0.9);
    b.set_pool(
        Some("gh".into()),
        Some(PoolShare {
            per_hour: 4500,
            kept_back: 900,
            wake_cap: 300,
            pool_sources: 2,
        }),
    );
    b.set_held(1);
    b
}

#[tokio::test]
async fn a_refusal_one_worker_gets_stops_the_other() {
    let api = Pool::default();
    let (a, b) = (worker(), worker());
    a.keep_unsent(Duration::from_secs(120));
    sync_once(&api, &a).await;
    assert!(a.admit(true).is_err());
    assert!(b.admit(true).is_ok(), "b has not asked the tracker yet");
    sync_once(&api, &b).await;
    assert!(b.admit(true).is_err(), "b stops at the pool's time");
    assert!(b.admit(false).is_err());
}

#[tokio::test]
async fn a_shorter_refusal_does_not_cut_the_pools_wait_short() {
    let api = Pool::default();
    let a = worker();
    a.keep_unsent(Duration::from_secs(600));
    sync_once(&api, &a).await;
    a.keep_unsent(Duration::from_secs(5));
    sync_once(&api, &a).await;
    let wait = a.admit(true).unwrap_err();
    assert!(wait > Duration::from_secs(500), "{wait:?}");
}

#[tokio::test]
async fn a_pool_wait_that_ended_lets_the_worker_go_again() {
    let api = Pool::default();
    let a = worker();
    a.keep_unsent(Duration::from_secs(60));
    sync_once(&api, &a).await;
    assert!(a.admit(true).is_err());
    *api.left.lock().unwrap() = Duration::ZERO;
    sync_once(&api, &a).await;
    assert!(a.admit(true).is_ok());
}

#[tokio::test]
async fn a_refusal_the_tracker_could_not_take_is_sent_again() {
    let api = Pool::default();
    let a = worker();
    a.keep_unsent(Duration::from_secs(60));
    *api.down.lock().unwrap() = true;
    sync_once(&api, &a).await;
    assert!(api.puts.lock().unwrap().is_empty());
    *api.down.lock().unwrap() = false;
    sync_once(&api, &a).await;
    assert_eq!(*api.puts.lock().unwrap(), [Duration::from_secs(60)]);
}

#[tokio::test]
async fn a_worker_with_no_pool_makes_no_call() {
    let api = Pool::default();
    let a = Budget::new(Vendor::Github, None, 0.9);
    a.keep_unsent(Duration::from_secs(60));
    sync_once(&api, &a).await;
    assert!(api.puts.lock().unwrap().is_empty());
}
