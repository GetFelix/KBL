use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;

use super::*;
use crate::github::Github;
use crate::source::Source;

#[test]
fn a_tail_keeps_the_last_bytes_of_every_chunk() {
    let mut t = Tail::new(8, 1 << 20);
    assert!(t.push(b"0123456789"));
    assert!(t.push(b"ab"));
    assert_eq!(t.finish(), "456789ab");
}

#[test]
fn a_tail_stops_at_its_cap_and_says_so() {
    let mut t = Tail::new(64, 10);
    assert!(t.push(b"0123456789"));
    assert!(!t.push(b"more"));
    let text = t.finish();
    assert!(text.ends_with("0123456789"), "{text}");
    assert!(text.contains("cut after 10 bytes"), "{text}");
    assert!(text.len() <= 64);

    // Exactly the cap is the whole log.
    let mut t = Tail::new(64, 10);
    assert!(t.push(b"0123456789"));
    assert_eq!(t.finish(), "0123456789");
}

#[test]
fn a_tail_decodes_bytes_lossily_and_starts_on_a_character() {
    // The last three bytes start inside the two-byte é.
    let mut t = Tail::new(3, 1 << 20);
    t.push("aébc".as_bytes());
    assert_eq!(t.finish(), "bc");

    let mut t = Tail::new(8, 1 << 20);
    t.push(&[0xff, b'z']);
    assert_eq!(t.finish(), "\u{fffd}z");
}

/// GitHub's job-log answer: a redirect to a signed link that serves bytes
/// and refuses a token.
async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let to = format!("{base}/signed/7001?sig=abc");
    let app = Router::new()
        .route(
            "/repos/GetFelix/felix/actions/jobs/7001/logs",
            get(move || async move { (StatusCode::FOUND, [(header::LOCATION, to)]) }),
        )
        .route(
            "/signed/7001",
            get(|h: HeaderMap| async move {
                if h.contains_key(header::AUTHORIZATION) {
                    return StatusCode::BAD_REQUEST.into_response();
                }
                let mut log = "é".repeat(50_000).into_bytes();
                log.extend_from_slice(b"\nerror: unused import\n");
                ([(header::CONTENT_TYPE, "application/octet-stream")], log).into_response()
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[tokio::test]
async fn a_job_log_is_streamed_through_the_redirect_as_bytes() {
    let api = serve().await;
    let dir = std::env::temp_dir().join(format!("kbl-joblog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("token"), "t0k3n").unwrap();
    let http = Reqwest::new(Token::File(dir.join("token"))).unwrap();
    let gh = Github::new(&api, http, Vec::new());
    let url = "https://github.com/GetFelix/felix/actions/runs/70/job/7001";
    let log = gh.check_log("GetFelix/felix", url, 4096).await.unwrap();
    assert!(log.len() <= 4096, "{}", log.len());
    assert!(log.ends_with("error: unused import\n"), "{log:?}");
    assert!(log.starts_with('é'), "{:?}", &log[..8]);
}
