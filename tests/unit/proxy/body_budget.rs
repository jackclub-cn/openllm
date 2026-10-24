use super::*;

#[tokio::test]
async fn admission_charges_actual_body_bytes_once_with_or_without_content_length() {
    for declared in [Some("600"), Some("900"), Some("300"), None] {
        let mut state = circuit_state().await;
        let budget = std::sync::Arc::new(RequestByteBudget::new(1024));
        state.inflight_request_bytes = Some(budget.clone());
        state.request_bytes_limit = 1024;
        state.memory_pressure = None;
        let router = axum::Router::new()
            .route(
                OPENAI_CHAT_COMPLETIONS,
                axum::routing::post(|body: Bytes| async move { body }),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                state,
                crate::proxy::admission,
            ));
        let chunks = futures_util::stream::iter(
            (0..3).map(|_| Ok::<_, io::Error>(Bytes::from(vec![b'x'; 200]))),
        );
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(OPENAI_CHAT_COMPLETIONS);
        if let Some(declared) = declared {
            request = request.header("content-length", declared);
        }
        let response = router
            .oneshot(request.body(Body::from_stream(chunks)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{declared:?}");
        assert_eq!(
            budget.used(),
            600,
            "the reservation must match actual buffered bytes for {declared:?}"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), vec![b'x'; 600]);
        assert_eq!(
            budget.used(),
            0,
            "response completion must release the bytes"
        );
    }
}
