mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn chat_completions_unknown_model_returns_404() {
    let (app, _, _dir) = common::setup_test_app();
    let body = json!({
        "model": "nonexistent",
        "messages": [{"role": "user", "content": "hi"}]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn chat_completions_echo_returns_200() {
    let (app, _, _dir) = common::setup_test_app();
    let body = json!({
        "model": "echo",
        "messages": [{"role": "user", "content": "Hello!"}]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
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
async fn chat_completions_with_instruction_override() {
    let (app, _, _dir) = common::setup_test_app();
    let body = json!({
        "model": "echo",
        "messages": [{"role": "system", "content": "You are helpful."}, {"role": "user", "content": "Hi"}]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
                .method("POST")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn chat_completions_streaming_returns_sse() {
    let (app, _, _dir) = common::setup_test_app();
    let body = json!({
        "model": "echo",
        "messages": [{"role": "user", "content": "Hi"}],
        "stream": true
    });
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/chat/completions")
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
    let text = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(text.contains("token"), "SSE should contain token events");
}
