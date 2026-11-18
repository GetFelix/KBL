//! The unauthenticated root answers with the service name, version and health route only.

use super::*;

#[tokio::test]
async fn the_root_names_the_service_without_a_token() {
    let app = app().await;
    let (status, body) = get_json(&app, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["service"], "kbl-tracker");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["health"], "/healthz");
    assert_eq!(body.as_object().unwrap().len(), 3, "{body}");
}
