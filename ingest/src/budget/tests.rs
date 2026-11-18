use serde_json::{Value, json};

use super::*;
use crate::testing::{Script, ok, status};

/// A clock the test moves by hand.
fn clock() -> (Clock, Arc<Mutex<Instant>>) {
    let now = Arc::new(Mutex::new(Instant::now()));
    let c = now.clone();
    (Arc::new(move || *c.lock().unwrap()), now)
}

fn advance(now: &Arc<Mutex<Instant>>, s: u64) {
    *now.lock().unwrap() += Duration::from_secs(s);
}

fn rig(
    vendor: Vendor,
    per_hour: Option<u64>,
) -> (Budgeted<Script>, Script, Arc<Budget>, Arc<Mutex<Instant>>) {
    let (c, now) = clock();
    let budget = Budget::with_clock(vendor, per_hour, 0.5, c);
    let script = Script::default();
    (
        budgeted(script.clone(), budget.clone()),
        script,
        budget,
        now,
    )
}

fn with_body(code: u16, headers: &[(&str, &str)], body: Value) -> Res {
    Res {
        body,
        ..status(code, headers)
    }
}

async fn get(t: &Budgeted<Script>) -> Result<Res> {
    t.send(Req::get("https://api/x".into())).await
}

fn waited(e: &anyhow::Error) -> Duration {
    e.downcast_ref::<Limited>().expect("a rate limit").wait
}

#[tokio::test]
async fn a_429_stops_every_request_until_retry_after() {
    let (t, script, _, now) = rig(Vendor::Github, None);
    script.on("GET", "/x", status(429, &[("retry-after", "120")]));
    assert_eq!(
        waited(&get(&t).await.unwrap_err()),
        Duration::from_secs(120)
    );
    advance(&now, 60);
    // Still held: no request leaves, whatever the path.
    assert_eq!(waited(&get(&t).await.unwrap_err()), Duration::from_secs(60));
    assert!(
        t.send(Req::post("https://api/y".into(), json!({})))
            .await
            .is_err()
    );
    assert_eq!(script.sent().len(), 1);
    advance(&now, 61);
    script.on("GET", "/x", ok(json!([])));
    assert!(get(&t).await.is_ok());
    assert_eq!(script.sent().len(), 2);
}

#[tokio::test]
async fn a_github_secondary_limit_403_backs_off_with_no_headers() {
    let (t, script, _, now) = rig(Vendor::Github, None);
    let body = json!({ "message": "You have exceeded a secondary rate limit." });
    script.on("GET", "/x", with_body(403, &[], body));
    assert_eq!(waited(&get(&t).await.unwrap_err()), Duration::from_secs(60));
    assert!(get(&t).await.is_err());
    assert_eq!(script.sent().len(), 1);
    advance(&now, 61);
    script.on("GET", "/x", ok(json!([])));
    assert!(get(&t).await.is_ok());
}

#[tokio::test]
async fn a_plain_403_is_not_a_rate_limit() {
    let (t, script, _, _) = rig(Vendor::Github, None);
    script.on("GET", "/x", status(403, &[]));
    assert_eq!(get(&t).await.unwrap().status, 403);
    assert!(get(&t).await.is_ok());
    assert_eq!(script.sent().len(), 2);
}

#[tokio::test]
async fn a_spent_primary_limit_holds_until_its_reset() {
    let (t, script, budget, now) = rig(Vendor::Github, None);
    let reset = (epoch() + 300).to_string();
    let headers = [
        ("x-ratelimit-limit", "5000"),
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", reset.as_str()),
    ];
    // The answer itself is good; the next request is what must wait.
    script.on(
        "GET",
        "/x",
        Res {
            body: json!([]),
            ..status(200, &headers)
        },
    );
    assert!(get(&t).await.is_ok());
    let wait = budget.admit(false).unwrap_err();
    assert!((299..=302).contains(&wait.as_secs()), "{wait:?}");
    assert!(get(&t).await.is_err());
    assert_eq!(script.sent().len(), 1);
    advance(&now, 303);
    assert!(get(&t).await.is_ok());
}

#[test]
fn a_wait_is_capped_at_an_hour() {
    let res = status(429, &[("retry-after", "86400")]);
    for v in [Vendor::Github, Vendor::Jira, Vendor::Ado, Vendor::Gitlab] {
        assert_eq!(signal(v, &res).wait, Some(MAX_BACKOFF), "{v:?}");
    }
}

#[test]
fn each_vendor_reads_its_own_signals() {
    // Jira: 429 with and without Retry-After.
    let s = signal(Vendor::Jira, &status(429, &[("retry-after", "7")]));
    assert!(s.refused && s.wait == Some(Duration::from_secs(7)));
    assert_eq!(
        signal(Vendor::Jira, &status(429, &[])).wait,
        Some(DEFAULT_WAIT)
    );
    assert!(!signal(Vendor::Jira, &status(200, &[])).refused);
    // Azure DevOps: Retry-After on a good answer, and a spent X-RateLimit window.
    let s = signal(Vendor::Ado, &status(200, &[("retry-after", "5")]));
    assert!(!s.refused && s.wait == Some(Duration::from_secs(5)));
    let reset = (epoch() + 40).to_string();
    let s = signal(
        Vendor::Ado,
        &status(
            200,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &reset),
            ],
        ),
    );
    assert!((40..=42).contains(&s.wait.unwrap().as_secs()));
    // GitLab: RateLimit-* with the limit per minute.
    let s = signal(
        Vendor::Gitlab,
        &status(
            200,
            &[
                ("ratelimit-limit", "100"),
                ("ratelimit-remaining", "3"),
                ("ratelimit-reset", "9"),
            ],
        ),
    );
    assert_eq!((s.hourly, s.remaining, s.wait), (Some(6000), Some(3), None));
    assert!(signal(Vendor::Gitlab, &status(429, &[])).refused);
}

#[tokio::test]
async fn the_bucket_throttles_polling_and_keeps_a_reserve_for_writes() {
    // 10 an hour: a reserve of 2, so reads stop once 3 tokens are left.
    let (t, script, budget, now) = rig(Vendor::Github, Some(10));
    script.on("GET", "/x", ok(json!([])));
    script.on("POST", "/y", ok(json!({})));
    let mut sent = 0;
    while get(&t).await.is_ok() {
        sent += 1;
        assert!(sent < 20, "never throttled");
    }
    assert_eq!(sent, 8);
    assert_eq!(script.sent().len(), 8);
    // A write still goes, from the reserve.
    assert!(
        t.send(Req::post("https://api/y".into(), json!({})))
            .await
            .is_ok()
    );
    // The write spent a reserve token; the wait is 360 s for each token missing.
    assert!(budget.poll_wait().unwrap() >= Duration::from_secs(300));
    advance(&now, 800);
    assert!(get(&t).await.is_ok());
}

#[tokio::test]
async fn a_conditional_github_listing_answered_304_is_not_charged() {
    let (t, script, budget, _) = rig(Vendor::Github, Some(10));
    script.on("GET", "/x", status(304, &[]));
    for _ in 0..30 {
        assert_eq!(get(&t).await.unwrap().status, 304);
    }
    assert_eq!(budget.snapshot().tokens, 10);
}

#[tokio::test]
async fn the_interval_stretches_to_fit_the_budget() {
    // 1000 an hour, 800 of it for polling. A poll of 200 reads fits 4 times
    // an hour, so 900 s apart, not the 60 s minimum.
    let (t, script, budget, _) = rig(Vendor::Github, Some(1000));
    script.on("GET", "/x", ok(json!([])));
    assert_eq!(
        budget.interval(Duration::from_secs(60)),
        Duration::from_secs(60)
    );
    budget.begin_poll();
    for _ in 0..200 {
        get(&t).await.unwrap();
    }
    budget.end_poll(true);
    assert_eq!(
        budget.interval(Duration::from_secs(60)),
        Duration::from_secs(900)
    );
    // A cheap poll keeps the minimum.
    budget.begin_poll();
    get(&t).await.unwrap();
    budget.end_poll(true);
    assert_eq!(
        budget.interval(Duration::from_secs(300)),
        Duration::from_secs(300)
    );
}

#[tokio::test]
async fn the_budget_is_a_share_of_what_the_vendor_reports() {
    let (t, script, budget, _) = rig(Vendor::Github, None);
    assert_eq!(budget.snapshot().hourly, 2500);
    let reset = (epoch() + 600).to_string();
    let headers = [
        ("x-ratelimit-limit", "15000"),
        ("x-ratelimit-remaining", "14000"),
        ("x-ratelimit-reset", reset.as_str()),
    ];
    script.on(
        "GET",
        "/x",
        Res {
            body: json!([]),
            ..status(200, &headers)
        },
    );
    get(&t).await.unwrap();
    let snap = budget.snapshot();
    assert_eq!(
        (snap.vendor_hourly, snap.vendor_remaining, snap.hourly),
        (Some(15000), Some(14000), 7500)
    );
}

#[test]
fn the_projection_counts_polls_and_one_pass() {
    // 4 calls a poll at 300 s is 12 polls an hour; 240 issues over 24 h is 10.
    let p = projected_per_hour(
        2,
        2,
        240,
        Duration::from_secs(300),
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(p.round() as u64, 58);
}

#[tokio::test]
async fn a_query_post_is_a_read_and_a_spent_window_does_not_stop_writes() {
    let (t, script, _, _) = rig(Vendor::Ado, Some(10));
    script.on("POST", "/_apis/wit/wiql", ok(json!({})));
    let q = || {
        Req::post(
            "https://dev/o/p/_apis/wit/wiql?api-version=7".into(),
            json!({}),
        )
    };
    let mut sent = 0;
    while t.send(q()).await.is_ok() {
        sent += 1;
        assert!(sent < 20);
    }
    assert_eq!(sent, 8, "queries draw on the poll budget");

    let (t, script, _, _) = rig(Vendor::Github, None);
    let reset = (epoch() + 3000).to_string();
    script.on(
        "GET",
        "/x",
        Res {
            body: json!([]),
            ..status(
                200,
                &[
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", &reset),
                ],
            )
        },
    );
    script.on("POST", "/y", ok(json!({})));
    get(&t).await.unwrap();
    assert!(get(&t).await.is_err());
    assert!(
        t.send(Req::post("https://api/y".into(), json!({})))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn an_aborted_poll_does_not_set_the_poll_cost() {
    let (t, script, budget, _) = rig(Vendor::Github, Some(1000));
    script.on("GET", "/x", ok(json!([])));
    budget.begin_poll();
    for _ in 0..200 {
        get(&t).await.unwrap();
    }
    budget.end_poll(false);
    assert_eq!(
        budget.interval(Duration::from_secs(60)),
        Duration::from_secs(60)
    );
}

#[tokio::test]
async fn a_mint_from_another_bucket_does_not_change_the_figures() {
    let (t, script, budget, _) = rig(Vendor::Github, None);
    script.on(
        "GET",
        "/x",
        status(
            200,
            &[
                ("x-ratelimit-limit", "5000"),
                ("x-ratelimit-remaining", "4000"),
                ("x-ratelimit-reset", "9999999999"),
                ("x-ratelimit-resource", "core"),
            ],
        ),
    );
    get(&t).await.unwrap();
    let before = budget.snapshot();
    let mint = |resource: &[(&str, &str)]| {
        let mut h = vec![
            ("x-ratelimit-limit", "15000"),
            ("x-ratelimit-remaining", "12"),
            ("x-ratelimit-reset", "1"),
        ];
        h.extend_from_slice(resource);
        status(201, &h)
    };
    for res in [
        mint(&[("x-ratelimit-resource", "integration_manifest")]),
        mint(&[]),
    ] {
        budget.metered(async { Ok(res) }).await.unwrap();
    }
    let after = budget.snapshot();
    assert_eq!(after.vendor_hourly, before.vendor_hourly);
    assert_eq!(after.vendor_remaining, Some(4000));
    assert_eq!(after.vendor_reset_epoch, before.vendor_reset_epoch);
    assert_eq!(after.hourly, before.hourly);
    // Each mint was still charged once.
    assert_eq!(after.tokens + 2, before.tokens);
    // The tracked bucket is adopted, and a refusal's wait is honoured either way.
    budget
        .metered(async { Ok(mint(&[("x-ratelimit-resource", "core")])) })
        .await
        .unwrap();
    assert_eq!(budget.snapshot().vendor_remaining, Some(12));
    let refused = status(403, &[("retry-after", "90")]);
    let e = budget.metered(async { Ok(refused) }).await.unwrap_err();
    assert_eq!(waited(&e), Duration::from_secs(90));
}

#[tokio::test]
async fn a_mint_before_any_normal_call_adopts_nothing() {
    let (_, _, budget, _) = rig(Vendor::Github, None);
    let res = status(
        201,
        &[
            ("x-ratelimit-remaining", "12"),
            ("x-ratelimit-resource", "core"),
        ],
    );
    budget.metered(async { Ok(res) }).await.unwrap();
    assert_eq!(budget.snapshot().vendor_remaining, None);
}

fn pool(per_hour: u64, kept_back: u64, wake_cap: u64, sources: u64) -> PoolShare {
    PoolShare {
        per_hour,
        kept_back,
        wake_cap,
        pool_sources: sources,
    }
}

#[test]
fn a_workers_share_follows_the_sources_it_holds_of_the_pool() {
    let (_, _, budget, _) = rig(Vendor::Github, Some(9999));
    budget.set_pool(Some("gh".into()), Some(pool(4500, 900, 300, 10)));
    budget.set_held(0);
    assert_eq!(budget.snapshot().hourly, 1, "no lease, no share");
    budget.set_held(4);
    assert_eq!(budget.snapshot().hourly, 1800, "4 of 10 sources of 4500");
    budget.set_held(10);
    assert_eq!(budget.snapshot().hourly, 4500);
    budget.set_held(25);
    assert_eq!(budget.snapshot().hourly, 4500, "never past the pool");
    budget.set_pool(None, None);
    assert_eq!(budget.snapshot().hourly, 9999, "the file's own setting");
}

#[test]
fn workers_holding_every_source_between_them_spend_no_more_than_the_pool() {
    let held = [3u64, 3, 2, 2];
    let total: u64 = held
        .iter()
        .map(|h| {
            let (_, _, b, _) = rig(Vendor::Github, None);
            b.set_pool(Some("gh".into()), Some(pool(4500, 900, 300, 10)));
            b.set_held(*h);
            b.snapshot().hourly
        })
        .sum();
    assert_eq!(total, 4500);
}

#[test]
fn the_pools_kept_back_share_sets_the_reserve() {
    let (_, _, budget, _) = rig(Vendor::Github, None);
    budget.set_pool(Some("gh".into()), Some(pool(1000, 500, 0, 1)));
    budget.set_held(1);
    // Half the bucket is kept back: a read needs tokens above 500.
    for _ in 0..500 {
        assert!(budget.admit(false).is_ok());
        budget.record(false, false, &status(200, &[]));
    }
    assert!(budget.admit(false).is_err(), "polling stops at the reserve");
    assert!(budget.admit(true).is_ok(), "writes still go");
}

#[test]
fn a_wake_spends_only_the_wake_part_of_the_reserve() {
    let (_, _, budget, now) = rig(Vendor::Github, None);
    // 3600 an hour, a fifth kept back, 360 of it for wakes: one a second.
    budget.set_pool(Some("gh".into()), Some(pool(3600, 720, 360, 1)));
    budget.set_held(1);
    // Polling drains the bucket to its reserve.
    while budget.admit(false).is_ok() {
        budget.record(false, false, &status(200, &[]));
    }
    assert!(budget.admit(true).is_ok());
    assert!(!budget.wake_affordable() || budget.snapshot().tokens >= 360);
    budget.begin_wake_poll();
    let mut spent = 0;
    while budget.admit(false).is_ok() {
        budget.record(false, false, &status(200, &[]));
        spent += 1;
        assert!(spent < 1000, "never stopped");
    }
    assert!(
        (300..=361).contains(&spent),
        "a wake got the wake part of the reserve, got {spent}"
    );
    // What writes keep is untouched.
    assert!(budget.snapshot().tokens >= 359);
    assert!(budget.admit(true).is_ok());
    advance(&now, 1);
}

#[test]
fn a_wake_that_the_wake_share_cannot_pay_for_waits_for_the_interval() {
    let (_, _, budget, now) = rig(Vendor::Github, None);
    budget.set_pool(Some("gh".into()), Some(pool(3600, 720, 60, 1)));
    budget.set_held(1);
    budget.begin_wake_poll();
    for _ in 0..60 {
        budget.record(false, false, &status(200, &[]));
    }
    budget.end_poll(true);
    assert!(!budget.wake_affordable(), "the wake share is spent");
    advance(&now, 3600);
    assert!(budget.wake_affordable(), "an hour earns it back");
}

#[tokio::test]
async fn another_workers_refusal_stops_this_worker_and_a_lifted_one_frees_it() {
    let (t, _, budget, now) = rig(Vendor::Github, None);
    budget.set_pool(Some("gh".into()), Some(pool(4500, 900, 300, 1)));
    budget.set_held(1);
    budget.set_pool_block(Duration::from_secs(90));
    let e = get(&t).await.unwrap_err();
    assert_eq!(waited(&e), Duration::from_secs(90));
    assert!(budget.admit(true).is_err(), "writes wait too");
    advance(&now, 91);
    assert!(budget.admit(false).is_ok());
    budget.set_pool_block(Duration::from_secs(90));
    budget.set_pool_block(Duration::ZERO);
    assert!(budget.admit(false).is_ok(), "the tracker lifted it");
}

#[tokio::test]
async fn a_refusal_is_kept_for_the_tracker_only_when_the_worker_has_a_pool() {
    let (t, script, budget, _) = rig(Vendor::Github, None);
    script.on("GET", "/x", status(429, &[("retry-after", "120")]));
    get(&t).await.unwrap_err();
    assert_eq!(
        budget.take_unsent(),
        None,
        "a file-managed worker has no pool"
    );
    budget.set_pool(Some("gh".into()), None);
    // The local block still holds; clear it with a fresh budget's clock.
    let (t, script, budget, _) = rig(Vendor::Github, None);
    budget.set_pool(Some("gh".into()), None);
    script.on("GET", "/x", status(429, &[("retry-after", "120")]));
    get(&t).await.unwrap_err();
    assert_eq!(budget.take_unsent(), Some(Duration::from_secs(120)));
    assert_eq!(budget.take_unsent(), None);
}
