//! With an endpoint set, a finished span reaches the collector as OTLP over
//! HTTP, from inside a Tokio runtime. A process of its own: `init` installs
//! the global subscriber.

use std::io::{Read, Write};
use std::net::TcpListener;

#[tokio::test(flavor = "multi_thread")]
async fn a_span_is_posted_to_the_traces_endpoint() {
    let collector = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = collector.local_addr().unwrap();
    let seen = std::thread::spawn(move || {
        let (mut conn, _) = collector.accept().unwrap();
        let mut head = vec![0u8; 4096];
        let n = conn.read(&mut head).unwrap();
        conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .unwrap();
        String::from_utf8_lossy(&head[..n]).to_string()
    });
    // SAFETY: the only test in this process, set before any other thread reads it.
    unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{addr}")) };

    let guard = kbl_trace::init("kbl-test");
    tracing::info_span!("one").in_scope(|| ());
    // Flushes the batch.
    drop(guard);

    let head = seen.join().unwrap();
    assert!(head.starts_with("POST /v1/traces "), "{head}");
}
