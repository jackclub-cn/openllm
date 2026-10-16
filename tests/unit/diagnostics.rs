use super::*;
use axum::extract::{Query, State};
use serde_json::json;

/// A state with one routable provider, one credential and one route target, so
/// the self-check has something healthy to look at.
async fn healthy_state() -> AppState {
    let state = empty_state().await;
    sqlx::query(
        "INSERT INTO providers (id, name, provider_type, base_url, headers, api_key) VALUES \
            (1, 'primary', 'openai', 'https://primary.example/v1', \
             '{\"Authorization\":\"Bearer sk-secret-value\",\"X-Custom\":\"keep-me\"}', \
             'sk-secret-value')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled) \
         VALUES (1, 1, 'Default', 'sk-secret-value', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_models (provider_id, model_name, enabled) VALUES (1, 'gpt-4o', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO routes (id, name, model_pattern, strategy, enabled) \
         VALUES (1, 'default', '*', 'priority', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO route_targets (route_id, provider_id, upstream_model, weight, priority, enabled) \
         VALUES (1, 1, 'gpt-4o', 100, 0, 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_prefix, key_suffix, key_hash, enabled) \
         VALUES (1, 'caller', 'sk-c', 'cccc', 'hash-c', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    state
}

/// A bare database with no providers, keys or routes at all.
async fn empty_state() -> AppState {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    AppState::new(pool, None)
}

fn item<'a>(report: &'a SelfCheckReport, id: &str) -> &'a SelfCheckItem {
    report
        .checks
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("self-check is missing the {id} item"))
}

/// Every item must be machine-readable: a stable id, a status from the fixed
/// set, and an actionable hint whenever the operator has to do something.
#[tokio::test]
async fn self_check_reports_structured_items() {
    let state = healthy_state().await;
    let report = run_self_check(&state).await;

    // The state under test always warns about process-local things (no lease
    // row in an in-memory database, unbounded capacity in a unit test), so the
    // assertion is that nothing reports a broken gateway.
    assert_ne!(report.overall, CHECK_FAIL, "{:#?}", report.checks);
    assert_eq!(report.counts.fail, 0, "{:#?}", report.checks);
    for id in [
        "database",
        "migrations",
        "provider_health",
        "provider_credentials",
        "routes",
        "gateway_keys",
    ] {
        assert_eq!(
            item(&report, id).status,
            CHECK_PASS,
            "{id} should pass on a healthy gateway: {:#?}",
            item(&report, id)
        );
    }
    assert_eq!(
        report.counts.pass + report.counts.warn + report.counts.fail,
        report.checks.len(),
    );

    let mut ids = std::collections::HashSet::new();
    for check in &report.checks {
        assert!(
            [CHECK_PASS, CHECK_WARN, CHECK_FAIL].contains(&check.status),
            "{} reported an unknown status {}",
            check.id,
            check.status
        );
        assert!(!check.category.is_empty(), "{} has no category", check.id);
        assert!(!check.detail.is_empty(), "{} has no detail", check.id);
        assert!(ids.insert(check.id), "duplicate self-check id {}", check.id);
        if check.status != CHECK_PASS {
            assert!(
                check.hint.is_some(),
                "{} is {} without a hint",
                check.id,
                check.status
            );
        }
    }

    // The categories are what a runbook indexes on.
    for category in [
        "startup",
        "resilience",
        "routing",
        "capacity",
        "observability",
    ] {
        assert!(
            report.checks.iter().any(|item| item.category == category),
            "no self-check item covers the {category} category"
        );
    }
}

/// A gateway that cannot route anything must say so loudly rather than report a
/// clean bill of health.
#[tokio::test]
async fn self_check_fails_when_nothing_can_route() {
    let state = empty_state().await;
    let report = run_self_check(&state).await;

    assert_eq!(report.overall, CHECK_FAIL);
    assert_eq!(item(&report, "provider_health").status, CHECK_FAIL);
    assert_eq!(item(&report, "routes").status, CHECK_FAIL);
    assert!(item(&report, "provider_health").hint.is_some());
}

/// A provider can legitimately carry no key at all (a local upstream) or carry
/// its credential in a custom header, so a missing key set is a warning and
/// never a failure.
#[tokio::test]
async fn missing_provider_credentials_is_a_warning_not_a_failure() {
    let state = healthy_state().await;
    sqlx::query("DELETE FROM provider_api_keys")
        .execute(&state.pool)
        .await
        .unwrap();

    let header_only = run_self_check(&state).await;
    assert_eq!(
        item(&header_only, "provider_credentials").status,
        CHECK_PASS
    );
    assert_eq!(header_only.counts.fail, 0);

    sqlx::query("UPDATE providers SET api_key = NULL, headers = '{}'")
        .execute(&state.pool)
        .await
        .unwrap();
    let none_at_all = run_self_check(&state).await;
    assert_eq!(
        item(&none_at_all, "provider_credentials").status,
        CHECK_WARN
    );
    assert!(item(&none_at_all, "provider_credentials").hint.is_some());
    assert_eq!(none_at_all.counts.fail, 0);
}

/// The snapshot is the file an operator attaches to a ticket, so it must not
/// carry a usable upstream credential unless asked.
#[tokio::test]
async fn snapshot_redacts_credentials_by_default() {
    let state = healthy_state().await;
    let Json(snapshot) = diagnostic_snapshot(State(state.clone()), Query(SnapshotQuery::default()))
        .await
        .unwrap();

    assert!(!snapshot.secrets_included);
    assert!(snapshot.secrets.is_none());
    assert!(!snapshot.self_check.checks.is_empty());

    let provider = snapshot
        .providers
        .iter()
        .find(|provider| provider.id == 1)
        .unwrap();
    assert!(provider.api_key_set);
    assert_eq!(provider.headers["Authorization"], json!("***"));
    assert_eq!(provider.headers["X-Custom"], json!("keep-me"));

    let text = serde_json::to_string(&snapshot).unwrap();
    assert!(
        !text.contains("sk-secret-value"),
        "the redacted snapshot leaked an upstream credential"
    );
    assert!(snapshot.notes.iter().any(|note| note.contains("明文")));
}

/// Including secrets is possible, but it is an explicit and audited action.
#[tokio::test]
async fn snapshot_includes_secrets_only_when_requested_and_audits_it() {
    let state = healthy_state().await;
    let Json(snapshot) = diagnostic_snapshot(
        State(state.clone()),
        Query(SnapshotQuery {
            include_secrets: true,
        }),
    )
    .await
    .unwrap();

    assert!(snapshot.secrets_included);
    let secrets = snapshot.secrets.as_ref().unwrap();
    assert_eq!(secrets.provider_credentials.len(), 1);
    assert_eq!(secrets.provider_credentials[0].secret, "sk-secret-value");
    assert_eq!(
        secrets.provider_headers[0].headers["Authorization"],
        json!("Bearer sk-secret-value"),
    );

    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'export' AND entity = 'diagnostics'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1, "a secret-bearing export must be audited");

    // The redacted path must not write an audit row.
    let Json(_) = diagnostic_snapshot(State(state.clone()), Query(SnapshotQuery::default()))
        .await
        .unwrap();
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'export' AND entity = 'diagnostics'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

/// A database carried forward from an older version can hold a provider secret
/// only in the legacy column; an export must not silently lose it.
#[tokio::test]
async fn snapshot_exports_a_legacy_only_provider_secret() {
    let state = healthy_state().await;
    sqlx::query("DELETE FROM provider_api_keys")
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(snapshot) = diagnostic_snapshot(
        State(state.clone()),
        Query(SnapshotQuery {
            include_secrets: true,
        }),
    )
    .await
    .unwrap();

    let secrets = snapshot.secrets.as_ref().unwrap();
    assert_eq!(secrets.provider_credentials.len(), 1);
    let legacy = &secrets.provider_credentials[0];
    assert_eq!(legacy.key_id, 0);
    assert_eq!(legacy.secret, "sk-secret-value");
    assert!(
        snapshot.notes.iter().any(|note| note.contains("旧版")),
        "the operator must be told a legacy credential was exported: {:?}",
        snapshot.notes
    );

    // A key that also exists in provider_api_keys is exported once, not twice.
    sqlx::query(
        "INSERT INTO provider_api_keys (id, provider_id, name, secret, enabled) \
         VALUES (1, 1, 'Default', 'sk-secret-value', 1)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let Json(snapshot) = diagnostic_snapshot(
        State(state.clone()),
        Query(SnapshotQuery {
            include_secrets: true,
        }),
    )
    .await
    .unwrap();
    let secrets = snapshot.secrets.as_ref().unwrap();
    assert_eq!(secrets.provider_credentials.len(), 1);
    assert_eq!(secrets.provider_credentials[0].key_id, 1);
    assert!(
        !snapshot.notes.iter().any(|note| note.contains("旧版")),
        "no legacy note when nothing legacy was exported: {:?}",
        snapshot.notes
    );
}

/// Prompt and completion previews are user content and never belong in a
/// snapshot, with or without secrets.
#[tokio::test]
async fn snapshot_never_carries_request_previews() {
    let state = healthy_state().await;
    sqlx::query(
        "INSERT INTO usage_logs (request_id, requested_model, endpoint, status_code, success, \
                                 request_preview, response_preview, error_message) \
         VALUES ('req-1', 'gpt-4o', 'chat', 502, 0, \
                 'PROMPT-MARKER', 'RESPONSE-MARKER', 'upstream refused')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    for include_secrets in [false, true] {
        let Json(snapshot) = diagnostic_snapshot(
            State(state.clone()),
            Query(SnapshotQuery { include_secrets }),
        )
        .await
        .unwrap();
        assert_eq!(snapshot.recent_failures.len(), 1);
        assert_eq!(
            snapshot.recent_failures[0].error_message.as_deref(),
            Some("upstream refused"),
        );
        assert_eq!(snapshot.requests_24h.total, 1);
        assert_eq!(snapshot.requests_24h.failed, 1);
        assert_eq!(snapshot.requests_24h.failures_by_status[0].status_code, 502);

        let text = serde_json::to_string(&snapshot).unwrap();
        assert!(!text.contains("PROMPT-MARKER"), "prompt text leaked");
        assert!(!text.contains("RESPONSE-MARKER"), "response text leaked");
    }
}

/// Long upstream error bodies are truncated so one snapshot cannot balloon.
#[tokio::test]
async fn snapshot_truncates_long_error_messages() {
    let state = healthy_state().await;
    let long = "x".repeat(MAX_SNAPSHOT_MESSAGE_CHARS + 50);
    sqlx::query(
        "INSERT INTO usage_logs (request_id, requested_model, endpoint, status_code, success, error_message) \
         VALUES ('req-long', 'gpt-4o', 'chat', 500, 0, ?)",
    )
    .bind(&long)
    .execute(&state.pool)
    .await
    .unwrap();

    let Json(snapshot) = diagnostic_snapshot(State(state), Query(SnapshotQuery::default()))
        .await
        .unwrap();
    let message = snapshot.recent_failures[0].error_message.as_ref().unwrap();
    assert_eq!(message.chars().count(), MAX_SNAPSHOT_MESSAGE_CHARS + 1);
    assert!(message.ends_with('…'));
}

/// Header redaction is name-based and case-insensitive, and it recurses so a
/// nested credential cannot ride along.
#[test]
fn redact_headers_masks_credential_names_recursively() {
    let redacted = redact_headers(&json!({
        "Authorization": "Bearer a",
        "x-api-key": "b",
        "X-CUSTOM": "keep",
        "nested": { "TOKEN": "c", "ok": 1 },
        "list": [{ "cookie": "d" }],
    }));

    assert_eq!(redacted["Authorization"], json!("***"));
    assert_eq!(redacted["x-api-key"], json!("***"));
    assert_eq!(redacted["X-CUSTOM"], json!("keep"));
    assert_eq!(redacted["nested"]["TOKEN"], json!("***"));
    assert_eq!(redacted["nested"]["ok"], json!(1));
    assert_eq!(redacted["list"][0]["cookie"], json!("***"));
}
