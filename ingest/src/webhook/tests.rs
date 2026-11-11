use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::*;

fn signed(secret: &[u8], body: &[u8]) -> String {
    format!("sha256={}", hex(&hmac_sha256(secret, body)))
}

async fn post(app: &Router, sig: Option<String>, body: &str) -> StatusCode {
    post_with(app, sig, body, None).await
}

async fn post_with(
    app: &Router,
    sig: Option<String>,
    body: &str,
    delivery: Option<&str>,
) -> StatusCode {
    let mut req = Request::post("/webhook");
    if let Some(d) = delivery {
        req = req.header("x-github-delivery", d);
    }
    if let Some(s) = sig {
        req = req.header("x-hub-signature-256", s);
    }
    app.clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
        .status()
}

#[test]
fn hmac_matches_the_published_test_vector() {
    // RFC 4231, test case 2.
    assert_eq!(
        hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[tokio::test]
async fn a_signed_webhook_wakes_the_poll_and_an_unsigned_one_does_not() {
    let wake = Arc::new(Wake::default());
    let app = router("s3cret".into(), wake.clone(), Duration::ZERO);
    let body = r#"{"action":"edited"}"#;
    assert_eq!(post(&app, None, body).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        post(&app, Some(signed(b"wrong", body.as_bytes())), body).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), wake.notify.notified())
            .await
            .is_err()
    );
    assert_eq!(
        post(&app, Some(signed(b"s3cret", body.as_bytes())), body).await,
        StatusCode::ACCEPTED
    );
    wake.notify.notified().await;
    assert!(!wake.reconcile.load(Ordering::SeqCst));
    // A redelivery is just another wake.
    assert_eq!(
        post(&app, Some(signed(b"s3cret", body.as_bytes())), body).await,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn a_transfer_asks_for_a_reconcile() {
    let wake = Arc::new(Wake::default());
    let app = router("k".into(), wake.clone(), Duration::ZERO);
    let body = r#"{"action":"transferred"}"#;
    post(&app, Some(signed(b"k", body.as_bytes())), body).await;
    assert!(wake.reconcile.load(Ordering::SeqCst));
}

async fn woken(wake: &Wake) -> bool {
    tokio::time::timeout(Duration::from_millis(20), wake.notify.notified())
        .await
        .is_ok()
}

#[tokio::test]
async fn a_repeated_delivery_id_is_dropped_and_a_burst_wakes_once() {
    let wake = Arc::new(Wake::default());
    let app = router("k".into(), wake.clone(), Duration::from_secs(3600));
    let body = r#"{"action":"transferred"}"#;
    let sig = || Some(signed(b"k", body.as_bytes()));
    assert_eq!(
        post_with(&app, sig(), body, Some("d1")).await,
        StatusCode::ACCEPTED
    );
    assert!(woken(&wake).await);
    // Another delivery inside the gap does not wake the poll.
    wake.reconcile.store(false, Ordering::SeqCst);
    assert_eq!(
        post_with(&app, sig(), body, Some("d2")).await,
        StatusCode::ACCEPTED
    );
    assert!(!woken(&wake).await);
    // But a transfer still asks for its reconcile.
    assert!(wake.reconcile.load(Ordering::SeqCst));
    // A redelivered id does nothing at all.
    wake.reconcile.store(false, Ordering::SeqCst);
    assert_eq!(
        post_with(&app, sig(), body, Some("d2")).await,
        StatusCode::ACCEPTED
    );
    assert!(!wake.reconcile.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_redelivery_does_not_wake_again_after_the_gap() {
    let wake = Arc::new(Wake::default());
    let app = router("k".into(), wake.clone(), Duration::ZERO);
    let body = r#"{"action":"edited"}"#;
    let sig = || Some(signed(b"k", body.as_bytes()));
    post_with(&app, sig(), body, Some("same")).await;
    assert!(woken(&wake).await);
    post_with(&app, sig(), body, Some("same")).await;
    assert!(!woken(&wake).await);
}

#[test]
fn the_delivery_list_is_bounded() {
    let mut r = Recent::default();
    for i in 0..SEEN_LIMIT + 10 {
        assert!(r.first_time(&i.to_string()));
    }
    assert_eq!(r.seen.len(), SEEN_LIMIT);
    // The oldest was forgotten; a recent one is not.
    assert!(r.first_time("0"));
    assert!(!r.first_time(&(SEEN_LIMIT + 9).to_string()));
}
