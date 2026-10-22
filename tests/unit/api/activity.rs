use super::*;
use crate::state::{ActivityDescription, ActivityFinish};

#[tokio::test]
async fn activity_api_exposes_live_and_recent_snapshots() {
    let state = provider_key_test_state().await;
    state.activity.start("activity-api", "/v1/chat/completions");
    state.activity.describe(
        "activity-api",
        ActivityDescription {
            requested_model: "model-a".to_string(),
            streamed: true,
            session_id: Some("session-a".to_string()),
            api_key_id: None,
            api_key_name: None,
            request_tokens_estimated: 20,
            context: None,
        },
    );

    let Json(snapshot) =
        get_request_activity(State(state.clone()), Path("activity-api".to_string()))
            .await
            .unwrap();
    assert_eq!(snapshot.request_id, "activity-api");
    assert_eq!(snapshot.outcome, "running");
    assert!(snapshot.streamed);

    state.activity.finish(
        "activity-api",
        ActivityFinish {
            status_code: 200,
            success: true,
            error_code: None,
            latency_ms: 100,
            first_token_ms: Some(40),
            usage: crate::models::Usage::new(20, 5),
        },
    );
    let Json(recent) = recent_activity(
        State(state.clone()),
        Query(RecentActivityQuery { limit: Some(10) }),
    )
    .await;
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].outcome, "success");
    assert_eq!(recent.active_count, 0);

    let error = get_request_activity(State(state), Path("missing".to_string()))
        .await
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)));
}

#[tokio::test]
async fn activity_routes_require_admin_auth_and_limit_recent_items() {
    let mut state = provider_key_test_state().await;
    state.admin_token = Some("test-admin".to_string());
    for index in 0..205 {
        state
            .activity
            .start(&format!("request-{index}"), "/v1/responses");
    }
    let router = crate::build_router(state.clone());
    for path in [
        "/api/activity/recent",
        "/api/activity/request-0",
        "/api/events",
    ] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/activity/recent?limit=999")
                .header("x-admin-token", "test-admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1000000)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 200);
    assert_eq!(body["active_count"], 205);
    assert_eq!(body["enabled"], true);
}

#[tokio::test]
async fn activity_sse_delivers_both_domains_and_resyncs_after_lag() {
    use futures_util::StreamExt;
    let state = provider_key_test_state().await;
    let ticket_response = event_stream_ticket(State(state.clone())).await.unwrap();
    let ticket_body = axum::body::to_bytes(ticket_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let ticket: Value = serde_json::from_slice(&ticket_body).unwrap();
    let ticket = ticket["ticket"].as_str().unwrap();
    let response = crate::build_router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/events?ticket={ticket}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body().into_data_stream();
    let ready = body.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&ready).contains("event: ready"));
    state
        .events
        .send(crate::state::UsageEvent {
            id: 1,
            request_id: "sse".to_string(),
            success: true,
            streamed: true,
        })
        .unwrap();
    state.activity.start("sse", "/v1/responses");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut usage = false;
        let mut activity = false;
        while !usage || !activity {
            let frame = body.next().await.unwrap().unwrap();
            let frame = String::from_utf8_lossy(&frame);
            usage |= frame.contains("event: usage");
            activity |= frame.contains("event: activity\n");
        }
    })
    .await
    .unwrap();
    for index in 0..1100 {
        state
            .activity
            .start(&format!("lag-{index}"), "/v1/responses");
    }
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&frame).contains("event: activity_resync"));
}
