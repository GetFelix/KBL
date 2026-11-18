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
