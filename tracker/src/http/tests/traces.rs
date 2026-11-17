//! Request spans are named by route template and carry no id, token or text,
//! and a `traceparent` from the caller makes the request its child.

use tracing::info_span;

use super::*;

/// Sends `body` to `path` with the given trace headers.
async fn call_traced(
    app: &Router,
    path: &str,
    token: &str,
    body: Value,
    trace: &axum::http::HeaderMap,
) -> StatusCode {
    let mut req = Request::post(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"));
    for (name, value) in trace.iter() {
        req = req.header(name, value);
    }
    app.clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_request_span_is_named_by_its_route_and_leaks_nothing() {
    let capture = kbl_trace::capture();
    let app = app().await;
    let secret_item = "secret-item-7f3a";
    let secret_text = "secret-text-9c1d";

    let (status, _) = get_json(
        &app,
        &format!("/v1/teams/default/work/items/{secret_item}"),
        Some("tok-gab"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "/v1/work/next", Some("tok-w1"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let _ = call(
        &app,
        "/v1/work/note",
        Some("tok-gab"),
        json!({ "item": secret_item, "text": secret_text }),
    )
    .await;

    let spans = capture.finished();
    let item = spans
        .iter()
        .find(|s| {
            s.attributes.get("http.route").map(String::as_str) == Some("/v1/work/items/{item}")
        })
        .unwrap_or_else(|| panic!("no span for the item route: {spans:#?}"));
    assert_eq!(item.name, "GET /v1/work/items/{item}");
    assert_eq!(item.attributes["http.request.method"], "GET");
    assert_eq!(item.attributes["http.response.status_code"], "409");

    let request = spans
        .iter()
        .find(|s| s.attributes.get("http.route").map(String::as_str) == Some("/v1/work/next"))
        .expect("the claim route has a span");
    let claim = spans
        .iter()
        .find(|s| s.name == "tracker.claim")
        .expect("a claim span");
    assert_eq!(
        claim.parent_id, request.span_id,
        "the claim is inside the request"
    );
    assert_eq!(claim.attributes["team"], "default");
    assert_eq!(claim.attributes["outcome"], "empty");

    for s in &spans {
        for (k, v) in &s.attributes {
            for secret in [secret_item, secret_text, "tok-gab", "tok-w1", "tok-ing"] {
                assert!(!v.contains(secret), "{secret} is in {} {k}: {s:#?}", s.name);
            }
        }
        assert!(!s.name.contains(secret_item), "{}", s.name);
    }
}

#[tokio::test]
async fn a_traceparent_joins_the_request_to_the_callers_span() {
    let capture = kbl_trace::capture();
    let app = app().await;
    let caller = info_span!("caller");
    // Sent from outside the span, so only the header can link the two.
    let trace = caller.in_scope(kbl_trace::headers);
    drop(caller);
    let status = call_traced(&app, "/v1/work/next", "tok-w1", json!({}), &trace).await;
    assert_eq!(status, StatusCode::OK);

    let spans = capture.finished();
    let caller = spans.iter().find(|s| s.name == "caller").unwrap();
    let request = spans
        .iter()
        .find(|s| s.attributes.get("http.route").map(String::as_str) == Some("/v1/work/next"))
        .unwrap();
    assert_eq!(request.trace_id, caller.trace_id);
    assert_eq!(request.parent_id, caller.span_id);
}

/// A call to `path` with a trace and the given token, if any, and the span it left.
async fn traced_root(path: &str, method: &str, token: Option<&str>) -> (StatusCode, bool) {
    let capture = kbl_trace::capture();
    let app = app().await;
    let caller = info_span!("caller");
    let trace = caller.in_scope(kbl_trace::headers);
    drop(caller);
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    for (name, value) in trace.iter() {
        req = req.header(name, value);
    }
    let body = if method == "POST" { "{}" } else { "" };
    let status = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status();
    let spans = capture.finished();
    let caller = spans.iter().find(|s| s.name == "caller").unwrap();
    let request = spans
        .iter()
        .find(|s| s.name.starts_with(method) && s.attributes.contains_key("http.route"))
        .unwrap_or_else(|| panic!("no request span: {spans:#?}"));
    (status, request.trace_id == caller.trace_id)
}

#[tokio::test]
async fn a_caller_without_a_valid_token_cannot_join_a_trace() {
    // Every route that has a request span, with a missing and a wrong token.
    for (method, path) in [
        ("GET", "/healthz"),
        ("GET", "/v1/me"),
        ("GET", "/v1/org/teams"),
        ("POST", "/v1/work/next"),
        ("GET", "/v1/work/items/x"),
        ("POST", "/v1/teams/default/work/next"),
        ("GET", "/v1/ingest/roots"),
    ] {
        for token in [None, Some("wrong")] {
            let (status, joined) = traced_root(path, method, token).await;
            assert!(
                !joined,
                "{method} {path} {token:?} joined the caller's trace"
            );
            if path != "/healthz" {
                assert!(status.is_client_error(), "{method} {path} {status}");
            }
        }
    }
}

#[tokio::test]
async fn a_valid_token_still_joins_on_every_authenticated_route() {
    for (method, path, token) in [
        ("GET", "/v1/me", "tok-gab"),
        ("POST", "/v1/work/next", "tok-w1"),
        ("GET", "/v1/ingest/roots", "tok-ing"),
    ] {
        let (_, joined) = traced_root(path, method, Some(token)).await;
        assert!(joined, "{method} {path} did not join the caller's trace");
    }
}
