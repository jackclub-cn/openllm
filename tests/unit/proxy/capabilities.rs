use super::*;

#[test]
fn rejects_estimated_input_above_route_context_limit() {
    let barrel = BarrelEnvelope {
        capabilities: Some(crate::models::ModelCapabilities {
            context_limit: Some(100),
            ..Default::default()
        }),
        incomplete: false,
        target_count: 1,
    };
    assert!(enforce_context_capacity(100, Some(&barrel)).is_ok());
    assert!(matches!(
        enforce_context_capacity(101, Some(&barrel)),
        Err(AppError::BadRequest(_))
    ));
    assert!(enforce_context_capacity(101, None).is_ok());
}

#[test]
fn rejects_estimated_input_above_model_input_limit_without_context_limit() {
    let barrel = BarrelEnvelope {
        capabilities: Some(crate::models::ModelCapabilities {
            input_limit: Some(100),
            ..Default::default()
        }),
        incomplete: false,
        target_count: 1,
    };
    assert!(enforce_context_capacity(100, Some(&barrel)).is_ok());
    assert!(matches!(
        enforce_context_capacity(101, Some(&barrel)),
        Err(AppError::BadRequest(_))
    ));
}

#[test]
fn effective_input_limit_prefers_the_stricter_value() {
    let capabilities = ModelCapabilities {
        context_limit: Some(100),
        input_limit: Some(200),
        ..Default::default()
    };
    assert_eq!(effective_input_limit(&capabilities), Some(100));
}

#[tokio::test]
async fn route_capability_overrides_cap_the_runtime_barrel() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Primary', 'openai', 'https://example.com/v1')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (
            provider_id, model_name, enabled, context_limit, input_limit, output_limit
         ) VALUES (1, 'model', 1, 100000, 80000, 20000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (
            id, name, model_pattern, strategy, enabled, context_override, output_override
         ) VALUES (1, 'capped', 'capped-model', 'priority', 1, 64000, 8000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model)
         VALUES (1, 1, 'model')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let resolved = resolve_route(&state, "capped-model", "/v1/chat/completions")
        .await
        .unwrap();
    let capabilities = resolved.barrel.unwrap().capabilities.unwrap();
    assert_eq!(capabilities.context_limit, Some(64_000));
    assert_eq!(capabilities.input_limit, Some(64_000));
    assert_eq!(capabilities.output_limit, Some(8_000));
}

#[test]
fn capability_headers_include_input_and_context_without_output_limit() {
    let mut response = Response::new(Body::empty());
    let receipt = Some(json!({
        "context_limit": 100,
        "input_limit": 80
    }));
    apply_capability_headers(&mut response, &receipt);
    assert_eq!(
        response
            .headers()
            .get("x-openllm-max-context-tokens")
            .and_then(|value| value.to_str().ok()),
        Some("100")
    );
    assert_eq!(
        response
            .headers()
            .get("x-openllm-max-input-tokens")
            .and_then(|value| value.to_str().ok()),
        Some("80")
    );
    assert!(
        response
            .headers()
            .get("x-openllm-max-output-tokens")
            .is_none()
    );
}

#[test]
fn clamps_requested_output_to_barrel_limit() {
    let barrel = barrel_with_output_limit(Some(8000));
    let mut body = json!({"model": "m", "max_tokens": 32000});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(8000));
    assert_eq!(body["max_tokens"], 8000);
}

#[test]
fn leaves_requests_within_the_barrel_untouched() {
    let barrel = barrel_with_output_limit(Some(8000));
    let mut body = json!({"model": "m", "max_tokens": 4096});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), None);
    assert_eq!(body["max_tokens"], 4096);

    // No known limit means there is nothing to clamp against.
    let unknown = barrel_with_output_limit(None);
    let mut body = json!({"model": "m", "max_tokens": 999999});
    assert_eq!(clamp_output_request(&mut body, Some(&unknown)), None);
    assert_eq!(body["max_tokens"], 999999);
}

#[test]
fn clamps_max_completion_tokens_too() {
    let barrel = barrel_with_output_limit(Some(1000));
    let mut body = json!({"model": "m", "max_completion_tokens": 5000});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(1000));
    assert_eq!(body["max_completion_tokens"], 1000);
}

#[test]
fn clamps_responses_max_output_tokens_too() {
    let barrel = barrel_with_output_limit(Some(1000));
    let mut body = json!({"model": "m", "max_output_tokens": 5000});
    assert_eq!(clamp_output_request(&mut body, Some(&barrel)), Some(1000));
    assert_eq!(body["max_output_tokens"], 1000);
    assert_eq!(
        requested_output_tokens_of(&json!({"max_output_tokens": 5000})),
        Some(5000)
    );
}

#[test]
fn receipt_reports_clamping_and_target_count() {
    let barrel = barrel_with_output_limit(Some(8000));
    let receipt = capability_receipt(Some(&barrel), Some(32000), Some(8000)).unwrap();
    assert_eq!(receipt["output_limit"], 8000);
    assert_eq!(receipt["requested_output_tokens"], 32000);
    assert_eq!(receipt["clamped_output_tokens"], 8000);
    assert_eq!(receipt["target_count"], 2);
    assert_eq!(receipt["limits_verified"], true);
}

#[test]
fn receipt_injected_only_into_json_objects() {
    let receipt = Some(json!({"output_limit": 8000}));
    let injected = inject_capability_receipt(br#"{"id":"x"}"#, &receipt).unwrap();
    let value: Value = serde_json::from_slice(&injected).unwrap();
    assert_eq!(value["capabilities"]["output_limit"], 8000);
    // Non-JSON bodies must be left alone rather than replaced.
    assert!(inject_capability_receipt(b"not json", &receipt).is_none());
    assert!(inject_capability_receipt(b"{}", &None).is_none());
}
