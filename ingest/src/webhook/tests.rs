use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::*;

fn signed(secret: &[u8], body: &[u8]) -> String {
    format!("sha256={}", hex(&hmac_sha256(secret, body)))
}

async fn post(app: &Router, sig: Option<String>, body: &str) -> StatusCode {
    let mut req = Request::post("/webhook");
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
    let app = router("s3cret".into(), wake.clone());
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
    let app = router("k".into(), wake.clone());
    let body = r#"{"action":"transferred"}"#;
    post(&app, Some(signed(b"k", body.as_bytes())), body).await;
    assert!(wake.reconcile.load(Ordering::SeqCst));
}
