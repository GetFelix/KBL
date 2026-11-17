//! A call to the tracker carries the current span's `traceparent`, and its own
//! span names the route, not anything in the body or the token.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::info_span;

use super::*;

/// Answers one request with `{}` and returns the head it received.
async fn one_reply(listener: TcpListener) -> String {
    let (mut conn, _) = listener.accept().await.unwrap();
    let mut buf = vec![0u8; 8192];
    let n = conn.read(&mut buf).await.unwrap();
    conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
        .await
        .unwrap();
    String::from_utf8_lossy(&buf[..n]).to_lowercase()
}

#[tokio::test]
async fn a_tracker_call_sends_the_traceparent_of_its_own_span() {
    let capture = kbl_trace::capture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(one_reply(listener));
    let http = Http::new(&base, "tok-secret-mcp".into()).unwrap();

    let tool = info_span!("mcp.tool");
    let _ = http
        .call("/v1/work/next", json!({ "text": "secret-body-text" }))
        .instrument(tool.clone())
        .await
        .unwrap();
    drop(tool);
    let head = server.await.unwrap();

    let spans = capture.finished();
    let tool = spans.iter().find(|s| s.name == "mcp.tool").unwrap();
    let call = spans.iter().find(|s| s.name == "tracker.request").unwrap();
    assert_eq!(call.trace_id, tool.trace_id);
    assert_eq!(call.parent_id, tool.span_id);
    assert_eq!(call.attributes["http.route"], "/v1/work/next");
    assert_eq!(call.attributes["http.response.status_code"], "200");
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
