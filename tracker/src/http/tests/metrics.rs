//! Request metrics are labelled by route template, method and status, and
//! nothing in a call's path, body or token reaches the exposition.

use super::*;

/// The sum of the request counters whose line holds every label in `has`.
fn requests(text: &str, has: &[&str]) -> f64 {
    text.lines()
        .filter(|l| l.starts_with("kbl_tracker_http_requests_total{"))
        .filter(|l| has.iter().all(|h| l.contains(h)))
        .filter_map(|l| l.rsplit_once(' ')?.1.parse::<f64>().ok())
        .sum()
}

#[tokio::test]
async fn requests_are_counted_by_route_template_and_leak_nothing() {
    let exporter = kbl_metrics::install("kbl-tracker-test");
    let app = app().await;
    let route = "route=\"/v1/work/items/{item}\"";
    let before = requests(&exporter.render(), &[route, "status=\"409\""]);

    let secret_item = "secret-item-7f3a";
    let (status, _) = get_json(
        &app,
        &format!("/v1/work/items/{secret_item}"),
        Some("tok-gab"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // The team-scoped form reaches the same route and is counted once.
    let (status, _) = get_json(
        &app,
        &format!("/v1/teams/default/work/items/{secret_item}"),
        Some("tok-gab"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = get_json(&app, "/v1/me", Some("not-a-token-xyz")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let secret_text = "secret-text-9c1d";
    let (status, _) = call(
        &app,
        "/v1/work/note",
        Some("tok-gab"),
        json!({ "item": secret_item, "text": secret_text }),
    )
    .await;
    assert_ne!(status, StatusCode::OK);

    let text = exporter.render();
    assert_eq!(
        requests(&text, &[route, "status=\"409\""]) - before,
        2.0,
        "{text}"
    );
    assert!(
        requests(&text, &["route=\"/v1/me\"", "status=\"401\""]) >= 1.0,
        "{text}"
    );
    assert!(
        text.contains("kbl_tracker_http_request_seconds_bucket{route=\"/v1/work/items/{item}\"")
    );
    for leaked in [
        secret_item,
        secret_text,
        "tok-w1",
        "tok-gab",
        "not-a-token-xyz",
        "ingest-gh",
    ] {
        assert!(
            !text.contains(leaked),
            "{leaked} is in the exposition:\n{text}"
        );
    }
}

#[tokio::test]
async fn the_agent_routes_have_one_label_whatever_the_agent_or_team() {
    let exporter = kbl_metrics::install("kbl-tracker-test");
    let app = app().await;
    let (status, _) = get_json(
        &app,
        "/v1/teams/default/agents/someone-named-zed",
        Some("tok-gab"),
    )
    .await;
    let text = exporter.render();
    assert!(
        requests(
            &text,
            &[
                "route=\"/v1/teams/{team}/agents/{*rest}\"",
                &format!("status=\"{}\"", status.as_u16())
            ]
        ) >= 1.0,
        "{text}"
    );
    assert!(!text.contains("someone-named-zed"), "{text}");
}
