//! The flake list routes: a person keeps the list, an agent cannot, and the
//! records wait for format 22.

use super::*;

async fn app_22(raise: bool) -> Router {
    let felix = Quiet::default();
    felix.keeps.store(true, Ordering::SeqCst);
    let tokens = r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{}}"#;
    let one = team("default", felix, tokens, 64).await;
    let app = router(Arc::new(Teams::new(vec![one]).unwrap()));
    if raise {
        raise_to(&app, 22).await;
    }
    app
}

fn entry() -> Value {
    json!({ "flake": "lint-timeout", "check": "lint", "pattern": "timed out", "issue": "GetFelix/felix#9" })
}

#[tokio::test]
async fn a_person_keeps_the_flake_list_after_the_raise() {
    let app = app_22(false).await;
    let (status, _) = call(&app, "/v1/work/flakes", Some("tok-gab"), entry()).await;
    assert_ne!(status, StatusCode::OK, "refused until the raise");

    let app = app_22(true).await;
    let (status, reply) = call(&app, "/v1/work/flakes", Some("tok-gab"), entry()).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (_, listed) = get_json(&app, "/v1/work/flakes", Some("tok-gab")).await;
    assert_eq!(listed["flakes"][0]["flake"], "lint-timeout");
    assert_eq!(listed["flakes"][0]["hits"], 0);

    let clear = json!({ "flake": "lint-timeout" });
    let (status, _) = call(&app, "/v1/work/flakes/clear", Some("tok-gab"), clear).await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = get_json(&app, "/v1/work/flakes", Some("tok-gab")).await;
    assert!(listed["flakes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn an_agent_cannot_change_or_read_the_flake_list() {
    let app = app_22(true).await;
    let (status, _) = call(&app, "/v1/work/flakes", Some("tok-w1"), entry()).await;
    assert_ne!(status, StatusCode::OK);
    let (status, _) = get_json(&app, "/v1/work/flakes", Some("tok-w1")).await;
    assert_ne!(status, StatusCode::OK);
}
