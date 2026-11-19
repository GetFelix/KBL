use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;

use super::*;

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
async fn a_job_log_is_streamed_from_its_signed_link_as_bytes() {
    let api = serve().await;
    let dir = std::env::temp_dir().join(format!("kbl-joblog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("token"), "t0k3n").unwrap();
    let http = Reqwest::new(Token::File(dir.join("token"))).unwrap();
    let logs = format!("{api}/repos/GetFelix/felix/actions/jobs/7001/logs");
    let res = http.send(Req::log(logs, 4096, false)).await.unwrap();
    assert_eq!(res.status, 302);
    // The adapter follows only https; the local server is plain http, so
    // the signed link is read here as the adapter would read it.
    let to = res.headers["location"].clone();
    let res = http.send(Req::log(to, 4096, true)).await.unwrap();
    assert_eq!(res.status, 200, "the signed link must not get the token");
    let log = res.body.as_str().unwrap();
    assert!(log.len() <= 4096, "{}", log.len());
    assert!(log.ends_with("error: unused import\n"), "{log:?}");
    assert!(log.starts_with('é'), "{:?}", &log[..8]);
}
