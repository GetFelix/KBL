use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::*;

#[tokio::test]
async fn the_endpoint_serves_what_was_recorded_and_the_process_basics() {
    let exporter = install("kbl-test");
    metrics::counter!("kbl_test_total", "result" => "ok").increment(3);
    let res = router(exporter)
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("kbl_test_total{result=\"ok\"} 3"), "{text}");
    assert!(text.contains("process_start_time_seconds"), "{text}");
    assert!(text.contains("kbl_uptime_seconds"), "{text}");
    assert!(
        text.contains("kbl_build_info{service=\"kbl-test\""),
        "{text}"
    );
}

#[tokio::test]
async fn nothing_but_metrics_is_served() {
    let res = router(install("kbl-test"))
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_non_loopback_listen_needs_the_flag() {
    let any: std::net::SocketAddr = "0.0.0.0:9100".parse().unwrap();
    let lo: std::net::SocketAddr = "127.0.0.1:9100".parse().unwrap();
    assert!(check_listen(lo, false, "--x").is_ok());
    let err = check_listen(any, false, "--x").unwrap_err().to_string();
    assert!(err.contains("--x"), "{err}");
    assert!(check_listen(any, true, "--x").is_ok());
}

#[test]
fn latency_and_lag_histograms_get_their_own_buckets() {
    let exporter = install("kbl-test");
    metrics::histogram!("kbl_test_lag_seconds").record(20.0);
    metrics::histogram!("kbl_test_latency_seconds").record(0.02);
    let text = exporter.render();
    assert!(
        text.contains("kbl_test_lag_seconds_bucket{le=\"60\"} 1"),
        "{text}"
    );
    assert!(
        text.contains("kbl_test_latency_seconds_bucket{le=\"0.025\"} 1"),
        "{text}"
    );
}
