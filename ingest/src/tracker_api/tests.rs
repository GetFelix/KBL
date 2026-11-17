//! Calls to the tracker carry the current span's `traceparent`, and their span
//! names the route template, not the source in the URL.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::info_span;

use super::*;

/// Answers one request with `{"state":null}` and returns the head it received.
async fn one_reply(listener: TcpListener) -> String {
    let (mut conn, _) = listener.accept().await.unwrap();
    let mut buf = vec![0u8; 8192];
    let n = conn.read(&mut buf).await.unwrap();
    let body = r#"{"state":null}"#;
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(reply.as_bytes()).await.unwrap();
    String::from_utf8_lossy(&buf[..n]).to_lowercase()
}

#[tokio::test]
async fn a_tracker_call_sends_the_traceparent_and_a_templated_route() {
    let capture = kbl_trace::capture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(one_reply(listener));
    let http = Http::new(&base, "tok-secret-ingest".into()).unwrap();

    let poll = info_span!("ingest.poll");
    http.load("github-secret-source")
        .instrument(poll.clone())
        .await
        .unwrap();
    drop(poll);
    let head = server.await.unwrap();

    let spans = capture.finished();
    let poll = spans.iter().find(|s| s.name == "ingest.poll").unwrap();
    let call = spans.iter().find(|s| s.name == "tracker.request").unwrap();
    assert_eq!(call.trace_id, poll.trace_id);
    assert_eq!(call.parent_id, poll.span_id);
    assert_eq!(call.attributes["http.route"], "/v1/ingest/state/{source}");
    assert!(
        head.contains(&format!(
            "traceparent: 00-{}-{}-",
            call.trace_id, call.span_id
        )),
        "{head}"
    );
    for v in call.attributes.values() {
        assert!(!v.contains("secret"), "{call:#?}");
    }
}
