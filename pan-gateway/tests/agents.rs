mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn list_agents_echo_is_available() {
    let (app, _, _dir) = common::setup_test_app();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    use http_body_util::BodyExt;
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let agents: Vec<String> = serde_json::from_slice(&body).unwrap();
    assert!(agents.contains(&"echo".to_string()));
}

#[tokio::test]
async fn agent_goals_echo_returns_200() {
    let (app, _, _dir) = common::setup_test_app();
    let body = json!({"objective": "Say something"});
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/agents/echo/goals")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    use http_body_util::BodyExt;
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(json.get("expressed").is_some());
}

#[tokio::test]
async fn agent_goals_unknown_agent_returns_404() {
    let (app, _, _dir) = common::setup_test_app();
    let body = json!({"objective": "x"});
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/agents/nonexistent/goals")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
