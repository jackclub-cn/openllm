use super::*;

#[test]
fn transient_upstream_4xx_only_matches_opaque_aggregator_errors() {
    // The trace-id-only aggregator error is transient and worth a retry.
    assert!(transient_upstream_4xx(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"{\"type\":\"invalid_request_error\",\"code\":\"\",\"message\":\"invalid request error trace_id: 63eeeadddaa2d5224a4acf42a78dd1de\"}\n","type":"invalid_request_error"}}"#
        ));
    // Actionable client errors carry a `param` and must not be retried.
    assert!(!transient_upstream_4xx(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"Too small: expected array to have >=1 items","type":"invalid_request_error","param":"input"}}"#
        ));
    // Success and other status classes are never treated as transient.
    assert!(!transient_upstream_4xx(
        StatusCode::OK,
        br#"{"error":{"message":"invalid request error trace_id: abc"}}"#
    ));
    assert!(!transient_upstream_4xx(
        StatusCode::BAD_REQUEST,
        br#"{"error":{"message":"context length exceeded"}}"#
    ));
}

#[test]
fn compat_thinking_history_error_is_retryable_across_targets() {
    assert!(compat_thinking_history_error(
        StatusCode::BAD_REQUEST,
        br#"{"error":{"message":"The `reasoning_text` in the thinking mode must be passed back to the API.","type":"invalid_request_error"}}"#
    ));
    assert!(compat_thinking_history_error(
        StatusCode::BAD_REQUEST,
        br#"{"error":{"message":"The `content[].thinking` in the thinking mode must be passed back to the API.","type":"invalid_request_error"}}"#
    ));
    assert!(!compat_thinking_history_error(
        StatusCode::BAD_REQUEST,
        br#"{"error":{"message":"missing model"}}"#
    ));
    assert!(!compat_thinking_history_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        br#"{"error":{"message":"The `reasoning_text` in the thinking mode must be passed back to the API."}}"#
    ));
}

#[test]
fn restates_quota_exhaustion_misreported_as_a_client_error() {
    // Chinese aggregators often use 403/400 for an empty balance.
    assert_eq!(
        effective_upstream_status(
            StatusCode::FORBIDDEN,
            r#"{"error":{"message":"用户额度不足"}}"#.as_bytes()
        ),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        effective_upstream_status(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#
        ),
        StatusCode::TOO_MANY_REQUESTS
    );
    // 402 is always a billing refusal, so it is retryable through fallback.
    assert_eq!(
        effective_upstream_status(StatusCode::PAYMENT_REQUIRED, b""),
        StatusCode::TOO_MANY_REQUESTS
    );
    // A genuine auth or request error must keep its status.
    assert_eq!(
        effective_upstream_status(
            StatusCode::FORBIDDEN,
            br#"{"error":{"message":"invalid API key"}}"#
        ),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        effective_upstream_status(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"message":"context length exceeded"}}"#
        ),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        effective_upstream_status(StatusCode::TOO_MANY_REQUESTS, b""),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[test]
fn extracts_trace_id_from_wrapped_upstream_error() {
    let body = br#"{"error":{"message":"{\"type\":\"invalid_request_error\",\"message\":\"invalid request error trace_id: b25620737c4014bef3c7d9d4b56e6854\"}\n","type":"invalid_request_error"}}"#;
    assert_eq!(
        upstream_trace_id(body).as_deref(),
        Some("b25620737c4014bef3c7d9d4b56e6854")
    );
    assert_eq!(
        upstream_trace_id(br#"{"error":{"message":"bad input"}}"#),
        None
    );
}

#[test]
fn retries_only_transient_statuses() {
    for status in [408, 409, 425, 429, 500, 502, 503] {
        assert!(
            retryable_status(StatusCode::from_u16(status).unwrap()),
            "{status} should be retryable"
        );
    }
    for status in [400, 401, 403, 404, 422] {
        assert!(
            !retryable_status(StatusCode::from_u16(status).unwrap()),
            "{status} should not be retryable"
        );
    }
}

#[test]
fn upstream_failure_classifier_separates_failure_scopes() {
    let target = endpoint_test_target("openai", None);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::SERVICE_UNAVAILABLE,
        br#"{"error":{"message":"upstream unavailable"}}"#,
    );
    assert_eq!(failure.scope, FailureScope::Provider);
    assert_eq!(failure.retry, RetryDecision::NextTarget);
    assert_eq!(failure.rest, RestAction::Target);

    let failure =
        classify_upstream_failure(&target, StatusCode::REQUEST_TIMEOUT, b"upstream timed out");
    assert_eq!(failure.scope, FailureScope::Transport);
    assert_eq!(failure.rest, RestAction::Target);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::UNAUTHORIZED,
        br#"{"error":{"message":"invalid api key"}}"#,
    );
    assert_eq!(failure.scope, FailureScope::Credential);
    assert_eq!(failure.retry, RetryDecision::DoNotRetry);
    assert_eq!(failure.rest, RestAction::Credential);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::UNAUTHORIZED,
        br#"{"error":{"message":"invalid API key for the content filter service"}}"#,
    );
    assert_eq!(failure.scope, FailureScope::Credential);
    assert_eq!(failure.reason_code, "credential_rejected");

    let failure = classify_upstream_failure(
        &target,
        StatusCode::FORBIDDEN,
        b"The request was rejected because it was considered high risk",
    );
    assert_eq!(failure.scope, FailureScope::RequestContent);
    assert_eq!(failure.reason_code, "content_policy_refusal");
    assert_eq!(failure.retry, RetryDecision::NextTarget);
    assert_eq!(failure.rest, RestAction::None);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::TOO_MANY_REQUESTS,
        br#"{"error":{"message":"input is too long; retry later"}}"#,
    );
    assert_eq!(failure.reason_code, "rate_limited");

    let failure = classify_upstream_failure(
        &target,
        StatusCode::BAD_REQUEST,
        b"prompt is too long: maximum context length exceeded",
    );
    assert_eq!(failure.scope, FailureScope::Model);
    assert_eq!(failure.reason_code, "context_limit_exceeded");
    assert_eq!(failure.retry, RetryDecision::NextTarget);
    assert_eq!(failure.rest, RestAction::None);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::NOT_FOUND,
        br#"{"error":{"message":"model not found"}}"#,
    );
    assert_eq!(failure.scope, FailureScope::Model);
    assert_eq!(failure.reason_code, "model_not_found");
    assert_eq!(failure.retry, RetryDecision::NextTarget);
    assert_eq!(failure.rest, RestAction::None);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::TOO_MANY_REQUESTS,
        br#"{"error":{"message":"monthly quota exhausted"}}"#,
    );
    assert_eq!(failure.scope, FailureScope::Credential);
    assert_eq!(failure.confidence, FailureConfidence::Exact);
    assert_eq!(failure.reason_code, "quota_exhausted");
    assert_eq!(failure.retry, RetryDecision::NextTarget);
    assert_eq!(failure.rest, RestAction::TargetAndCredential);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::TOO_MANY_REQUESTS,
        br#"{"error":{"message":"rate limit reached"}}"#,
    );
    assert_eq!(failure.confidence, FailureConfidence::Inferred);
    assert_eq!(failure.reason_code, "rate_limited");
    assert_eq!(failure.retry, RetryDecision::WaitThenRetry);
    assert_eq!(failure.rest, RestAction::None);
    assert!(failure.should_sink());
    assert!(failure.should_retry_next_target());

    let failure = classify_upstream_failure(
        &target,
        StatusCode::CONFLICT,
        br#"{"error":{"message":"request state conflict"}}"#,
    );
    assert_eq!(failure.scope, FailureScope::RequestContent);
    assert_eq!(failure.reason_code, "request_conflict");
    assert_eq!(failure.retry, RetryDecision::NextTarget);
    assert_eq!(failure.rest, RestAction::None);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::BAD_REQUEST,
        br#"{"error":{"message":"invalid parameter"}}"#,
    );
    assert_eq!(failure.scope, FailureScope::RequestShape);
    assert_eq!(failure.retry, RetryDecision::DoNotRetry);
    assert_eq!(failure.rest, RestAction::None);

    let failure = classify_upstream_failure(
        &target,
        StatusCode::BAD_REQUEST,
        b"The reasoning_text in the thinking mode must be passed back to the API",
    );
    assert_eq!(failure.scope, FailureScope::RequestShape);
    assert_eq!(failure.reason_code, "request_shape_incompatible");
    assert_eq!(failure.retry, RetryDecision::NextTarget);
}

#[tokio::test]
async fn provider_failures_do_not_rest_the_selected_credential() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.provider_api_key_id = Some(11);

    let failure = record_upstream_failure(
        &state,
        &target,
        StatusCode::SERVICE_UNAVAILABLE,
        &HeaderMap::new(),
        "upstream unavailable",
    )
    .await;

    assert_eq!(failure.scope, FailureScope::Provider);
    assert!(
        state
            .target_cooldown
            .lock()
            .await
            .contains_key(&(7, target.upstream_model.clone()))
    );
    assert!(
        !state.provider_key_cooldown.lock().await.contains_key(&11),
        "a provider-wide failure must not mark the selected key unhealthy"
    );
}

#[tokio::test]
async fn content_policy_failures_do_not_rest_any_local_target() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let state = AppState::new(pool, None);
    let mut target = endpoint_test_target("openai", None);
    target.provider_id = 7;
    target.provider_api_key_id = Some(11);

    let failure = record_upstream_failure(
        &state,
        &target,
        StatusCode::FORBIDDEN,
        &HeaderMap::new(),
        "The request was rejected because it was considered high risk",
    )
    .await;

    assert_eq!(failure.scope, FailureScope::RequestContent);
    assert_eq!(failure.retry, RetryDecision::NextTarget);
    assert!(state.target_cooldown.lock().await.is_empty());
    assert!(state.provider_key_cooldown.lock().await.is_empty());
    assert!(state.provider_cooldown.lock().await.is_empty());
}

#[test]
fn same_target_retry_skips_rate_limits_and_client_errors() {
    for status in [408, 425, 500, 502, 503, 504, 529] {
        assert!(
            retryable_same_target_status(StatusCode::from_u16(status).unwrap()),
            "{status} should be retried against the same target"
        );
    }
    // A rate limit is handled by the cooldown/fallback machinery instead.
    for status in [400, 401, 403, 404, 409, 422, 429] {
        assert!(
            !retryable_same_target_status(StatusCode::from_u16(status).unwrap()),
            "{status} should not be retried against the same target"
        );
    }
}

#[test]
fn retry_backoff_is_exponential_and_capped() {
    let settings = crate::models::ResilienceSettings {
        stream_recovery_enabled: false,
        stream_recovery_max_retries: 2,
        max_retries: 3,
        retry_backoff_ms: 100,
        retry_max_backoff_ms: 400,
    };
    assert_eq!(
        retry_backoff_with_draw(&settings, 0, 0.0),
        Duration::from_millis(100)
    );
    assert_eq!(
        retry_backoff_with_draw(&settings, 0, 1.0),
        Duration::from_millis(125)
    );
    assert_eq!(
        retry_backoff_with_draw(&settings, 1, 0.4),
        Duration::from_millis(220)
    );
    assert_eq!(
        retry_backoff_with_draw(&settings, 2, 1.0),
        Duration::from_millis(400)
    );
    assert_eq!(
        retry_backoff_with_draw(&settings, 9, 1.0),
        Duration::from_millis(400)
    );

    let disabled = crate::models::ResilienceSettings {
        stream_recovery_enabled: false,
        stream_recovery_max_retries: 2,
        max_retries: 0,
        retry_backoff_ms: 0,
        retry_max_backoff_ms: 0,
    };
    assert_eq!(
        retry_backoff_with_draw(&disabled, 0, 1.0),
        Duration::from_millis(0)
    );
}

#[test]
fn auth_failures_only_fall_through_when_another_candidate_exists() {
    let mut target = endpoint_test_target("openai", None);
    assert!(!should_try_next_target(&target, StatusCode::UNAUTHORIZED));
    target.auth_retryable = true;
    assert!(should_try_next_target(&target, StatusCode::UNAUTHORIZED));
    assert!(should_try_next_target(&target, StatusCode::FORBIDDEN));
    assert!(!should_try_next_target(&target, StatusCode::BAD_REQUEST));
}

#[test]
fn provider_reachable_status_keeps_service_failures_open() {
    assert!(provider_reachable_status(StatusCode::BAD_REQUEST));
    assert!(provider_reachable_status(StatusCode::UNAUTHORIZED));
    assert!(provider_reachable_status(StatusCode::CONFLICT));
    assert!(provider_reachable_status(StatusCode::TOO_MANY_REQUESTS));
    assert!(!provider_reachable_status(StatusCode::REQUEST_TIMEOUT));
    assert!(!provider_reachable_status(StatusCode::BAD_GATEWAY));
    assert!(!provider_reachable_status(StatusCode::SERVICE_UNAVAILABLE));
}

#[test]
fn upstream_rate_limit_errors_keep_status_and_retry_after() {
    // Sub-second windows round up to 1 so a client never reads "0".
    let openai = AppError::UpstreamStatus {
        status: StatusCode::TOO_MANY_REQUESTS,
        message: "mock returned 429".to_string(),
        retry_after: Some(Duration::from_millis(500)),
    }
    .into_response();
    assert_eq!(openai.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        openai.headers().get(reqwest::header::RETRY_AFTER).unwrap(),
        "1"
    );

    let anthropic = anthropic_error_response(AppError::UpstreamStatus {
        status: StatusCode::TOO_MANY_REQUESTS,
        message: "mock returned 429".to_string(),
        retry_after: Some(Duration::from_secs(30)),
    });
    assert_eq!(anthropic.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        anthropic
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .unwrap(),
        "30"
    );

    // A non-429 status must not grow a Retry-After header from nothing.
    let plain = AppError::Upstream("upstream exploded".to_string()).into_response();
    assert_eq!(plain.status(), StatusCode::BAD_GATEWAY);
    assert!(plain.headers().get(reqwest::header::RETRY_AFTER).is_none());
}
