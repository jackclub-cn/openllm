use super::*;

#[tokio::test]
async fn audit_log_records_mutations_and_supports_filters() {
    use tower::ServiceExt;

    let state = provider_key_test_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url)
         VALUES (1, 'Audit provider', 'openai', 'https://audit.example/v1')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let router = crate::build_router(state.clone());

    // A real mutation through the admin API must leave an audit trail.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/routes")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Audited route",
                        "model_pattern": "audit-*",
                        "strategy": "priority",
                        "enabled": true,
                        "targets": [
                            {
                                "provider_id": 1,
                                "upstream_model": "m",
                                "weight": 100,
                                "priority": 0,
                                "enabled": true,
                            }
                        ],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // A failed mutation must not be recorded as a change.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/routes/9999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/audit-logs?entity=route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(page["total"], 1, "{page}");
    assert_eq!(page["items"][0]["action"], "create");
    assert_eq!(page["items"][0]["entity"], "route");
    assert_eq!(page["items"][0]["summary"], "created route 'Audited route'");
    assert_eq!(page["items"][0]["detail"]["strategy"], "priority");

    // Search matches the summary and entity id fields.
    let Json(page) = list_audit_logs(
        State(state.clone()),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: None,
            action: None,
            search: Some("Audited".to_string()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(page.total, 1);

    // The CSV export honours the same filters and carries a header row.
    let response = export_audit_logs(
        State(state.clone()),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: Some("route".to_string()),
            action: None,
            search: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/csv; charset=utf-8"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let csv = String::from_utf8(body.to_vec()).unwrap();
    assert!(csv.starts_with('\u{feff}'));
    assert!(csv.contains("created_at,id,action,entity,entity_id,actor,summary,detail"));
    assert!(csv.contains("created route 'Audited route'"));
    assert!(
        !csv.contains("provider"),
        "the filter must exclude other entities"
    );

    let Json(page) = list_audit_logs(
        State(state),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: Some("provider".to_string()),
            action: None,
            search: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(page.total, 0);
}

/// A capped export must announce the truncation so the console warns instead of
/// handing the operator a silently incomplete file.
#[tokio::test]
async fn audit_export_flags_truncation_at_the_row_cap() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    sqlx::query(
        "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 5001) \
         INSERT INTO audit_logs (action, entity, summary, actor) \
         SELECT 'update', 'settings', 'row ' || n, 'local' FROM seq",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::new(pool, None);

    let response = export_audit_logs(
        State(state),
        Query(AuditLogQuery {
            page: None,
            page_size: None,
            entity: None,
            action: None,
            search: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-openllm-export-truncated"], "true");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let csv = String::from_utf8(body.to_vec()).unwrap();
    // The header row plus exactly the row cap of data rows.
    assert_eq!(csv.lines().count(), 1 + 5000);
}
