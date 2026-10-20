use super::*;

#[derive(Debug, Default, serde::Deserialize)]
pub struct SnapshotQuery {
    /// Opt-in: include upstream provider credentials in plaintext. Off by
    /// default so a snapshot can be attached to a ticket safely.
    #[serde(default)]
    pub include_secrets: bool,
}

/// A support-ready bundle of everything an operator has to gather by hand today.
#[derive(Debug, serde::Serialize)]
pub struct DiagnosticSnapshot {
    pub generated_at: String,
    pub version: &'static str,
    pub host: String,
    pub draining: bool,
    pub uptime_seconds: u64,
    pub instance_lease: Option<LeaseView>,
    pub status: RuntimeStatus,
    pub limits: RuntimeLimits,
    pub settings: SettingsView,
    pub self_check: SelfCheckReport,
    pub resilience: CooldownSnapshot,
    pub pools: Vec<crate::allocation::PoolUsageView>,
    pub providers: Vec<ProviderView>,
    pub recent_failures: Vec<FailureView>,
    pub requests_24h: RequestOutcomeSummary,
    /// Whether `secrets` is populated. Always explicit so a reader can tell a
    /// redacted file from a secret-bearing one.
    pub secrets_included: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<SnapshotSecrets>,
    pub notes: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct LeaseView {
    pub id: String,
    pub pid: i64,
    pub host: String,
    pub started_at: String,
    pub heartbeat_at: String,
    pub heartbeat_age_seconds: i64,
    /// Whether the lease belongs to this host (the only supported topology).
    pub ours: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct FailureView {
    pub request_id: String,
    pub created_at: String,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub provider_id: Option<i64>,
    pub endpoint: String,
    pub streamed: bool,
    pub status_code: i64,
    pub latency_ms: i64,
    pub error_message: Option<String>,
    pub warning_message: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct RequestOutcomeSummary {
    pub total: i64,
    pub successful: i64,
    pub failed: i64,
    /// Failed requests per HTTP status code, most frequent first.
    pub failures_by_status: Vec<StatusCount>,
}

#[derive(Debug, serde::Serialize)]
pub struct StatusCount {
    pub status_code: i64,
    pub count: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct SnapshotSecrets {
    pub provider_credentials: Vec<ProviderCredentialSecret>,
    pub provider_headers: Vec<ProviderHeaderSecret>,
}

#[derive(Debug, serde::Serialize)]
pub struct ProviderCredentialSecret {
    pub provider_id: i64,
    pub provider_name: String,
    pub key_id: i64,
    pub key_name: String,
    pub secret: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ProviderHeaderSecret {
    pub provider_id: i64,
    pub provider_name: String,
    pub headers: Value,
}

/// Exports the diagnostic snapshot.
///
/// Redaction is the default: upstream credentials and provider headers are
/// dropped, and request/response previews never enter the file at all. Passing
/// `include_secrets=true` is an explicit, audited action.
pub async fn diagnostic_snapshot(
    State(state): State<AppState>,
    Query(query): Query<SnapshotQuery>,
) -> AppResult<Json<DiagnosticSnapshot>> {
    let limits = state.runtime_limits();
    let generated_at = Utc::now().to_rfc3339();

    let mut providers = sqlx::query_as::<_, Provider>(
        "SELECT * FROM providers ORDER BY enabled DESC, name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(ProviderView::from)
    .collect::<Vec<_>>();
    hydrate_provider_views(&state, &mut providers).await?;
    let raw_headers = providers
        .iter()
        .map(|provider| (provider.id, provider.name.clone(), provider.headers.clone()))
        .collect::<Vec<_>>();
    if !query.include_secrets {
        for provider in &mut providers {
            provider.headers = redact_headers(&provider.headers);
        }
    }

    let lease = load_lease_view(&state).await;
    let (secrets, legacy_secrets) = if query.include_secrets {
        let (secrets, legacy) = load_secrets(&state, raw_headers).await?;
        record_audit(
            &state,
            "export",
            "diagnostics",
            None,
            "导出了包含明文上游凭据的诊断快照",
            None,
        )
        .await;
        (Some(secrets), legacy)
    } else {
        (None, 0)
    };

    let notes = if query.include_secrets {
        let mut notes = vec![
            "本文件包含明文上游凭据，请按机密文件处理，用完即删。".to_string(),
            "请求与响应预览、提示词内容不在快照内。".to_string(),
        ];
        if legacy_secrets > 0 {
            notes.push(format!(
                "另有 {legacy_secrets} 条凭据仅存在于旧版 providers.api_key 字段，以 key_id=0 导出。"
            ));
        }
        notes
    } else {
        vec![
            "上游凭据仅以 api_key_set/后缀形式出现，不含明文；带上 include_secrets=true 才会导出明文（会记入审计日志）。".to_string(),
            "请求与响应预览、提示词内容不在快照内。".to_string(),
        ]
    };

    Ok(Json(DiagnosticSnapshot {
        generated_at,
        version: env!("CARGO_PKG_VERSION"),
        host: crate::startup::host_name(),
        draining: state.shutting_down.load(Relaxed),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        instance_lease: lease,
        status: collect_status(&state).await,
        limits,
        settings: SettingsView {
            admin_auth_enabled: state.admin_token.is_some(),
            database: "sqlite",
            version: env!("CARGO_PKG_VERSION"),
            database_stats: load_database_stats(&state).await?,
        },
        self_check: run_self_check(&state).await,
        resilience: cooldown_snapshot(&state).await?,
        pools: crate::allocation::all_pool_usage(&state)
            .await
            .unwrap_or_default(),
        providers,
        recent_failures: load_recent_failures(&state).await,
        requests_24h: load_request_outcomes(&state).await,
        secrets_included: secrets.is_some(),
        secrets,
        notes,
    }))
}

pub(super) async fn load_lease_view(state: &AppState) -> Option<LeaseView> {
    let host = crate::startup::host_name();
    sqlx::query_as::<_, (String, i64, String, String, String, i64)>(
        "SELECT id, pid, host, started_at, heartbeat_at, \
                CAST((julianday('now') - julianday(heartbeat_at)) * 86400 AS INTEGER) \
         FROM instance_leases ORDER BY heartbeat_at DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .map(
        |(id, pid, lease_host, started_at, heartbeat_at, heartbeat_age_seconds)| LeaseView {
            ours: lease_host == host,
            id,
            pid,
            host: lease_host,
            started_at,
            heartbeat_at,
            heartbeat_age_seconds,
        },
    )
}

pub(super) async fn load_recent_failures(state: &AppState) -> Vec<FailureView> {
    sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            Option<i64>,
            i64,
            i64,
            Option<String>,
            Option<String>,
            String,
            i64,
        ),
    >(
        "SELECT request_id, created_at, requested_model, upstream_model, provider_id, \
                status_code, latency_ms, error_message, warning_message, endpoint, streamed \
         FROM usage_logs WHERE success = 0 AND in_flight = 0 ORDER BY id DESC LIMIT ?",
    )
    .bind(MAX_SNAPSHOT_FAILURES)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(
        |(
            request_id,
            created_at,
            requested_model,
            upstream_model,
            provider_id,
            status_code,
            latency_ms,
            error_message,
            warning_message,
            endpoint,
            streamed,
        )| FailureView {
            request_id,
            created_at,
            requested_model,
            upstream_model,
            provider_id,
            endpoint,
            streamed: streamed != 0,
            status_code,
            latency_ms,
            error_message: truncate_message(error_message),
            warning_message: truncate_message(warning_message),
        },
    )
    .collect()
}

pub(super) async fn load_request_outcomes(state: &AppState) -> RequestOutcomeSummary {
    let totals = sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*), COALESCE(SUM(success), 0) FROM usage_logs \
         WHERE in_flight = 0 \
           AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day')",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or((0, 0));
    let failures_by_status = sqlx::query_as::<_, (i64, i64)>(
        "SELECT status_code, COUNT(*) FROM usage_logs \
         WHERE success = 0 AND in_flight = 0 \
           AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day') \
         GROUP BY status_code ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(status_code, count)| StatusCount { status_code, count })
    .collect();
    RequestOutcomeSummary {
        total: totals.0,
        successful: totals.1,
        failed: totals.0.saturating_sub(totals.1),
        failures_by_status,
    }
}

pub(super) async fn load_secrets(
    state: &AppState,
    headers: Vec<(i64, String, Value)>,
) -> AppResult<(SnapshotSecrets, i64)> {
    let mut provider_credentials = sqlx::query_as::<_, (i64, String, i64, String, String)>(
        "SELECT p.id, p.name, k.id, k.name, k.secret \
         FROM provider_api_keys k JOIN providers p ON p.id = k.provider_id \
         ORDER BY p.id, k.id",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(
        |(provider_id, provider_name, key_id, key_name, secret)| ProviderCredentialSecret {
            provider_id,
            provider_name,
            key_id,
            key_name,
            secret,
        },
    )
    .collect::<Vec<_>>();
    // Any key created through the API is mirrored into provider_api_keys, but a
    // database carried forward from an older version can still hold a secret in
    // the legacy column only. Export it too (key_id 0) so an operator never
    // restores a provider from a snapshot without its credential.
    let legacy = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, name, api_key FROM providers \
         WHERE COALESCE(api_key, '') <> '' ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut legacy_exported = 0;
    for (provider_id, provider_name, secret) in legacy {
        if provider_credentials
            .iter()
            .any(|item| item.provider_id == provider_id && item.secret == secret)
        {
            continue;
        }
        legacy_exported += 1;
        provider_credentials.push(ProviderCredentialSecret {
            provider_id,
            provider_name,
            key_id: 0,
            key_name: "旧版 api_key 字段".to_string(),
            secret,
        });
    }
    let provider_headers = headers
        .into_iter()
        .map(
            |(provider_id, provider_name, headers)| ProviderHeaderSecret {
                provider_id,
                provider_name,
                headers,
            },
        )
        .collect();
    Ok((
        SnapshotSecrets {
            provider_credentials,
            provider_headers,
        },
        legacy_exported,
    ))
}

pub(super) fn truncate_message(message: Option<String>) -> Option<String> {
    message.map(|message| {
        if message.chars().count() <= MAX_SNAPSHOT_MESSAGE_CHARS {
            message
        } else {
            let head = message
                .chars()
                .take(MAX_SNAPSHOT_MESSAGE_CHARS)
                .collect::<String>();
            format!("{head}…")
        }
    })
}

/// Replaces the value of any header that looks like a credential.
///
/// Providers let operators add arbitrary upstream headers, and a gateway
/// operator may well have put a token there. Keys are matched
/// case-insensitively on the usual credential names; everything else is kept so
/// the snapshot stays useful.
pub(crate) fn redact_headers(headers: &Value) -> Value {
    match headers {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(name, value)| {
                    if is_secret_header(name) {
                        (name.clone(), Value::String("***".to_string()))
                    } else {
                        (name.clone(), redact_headers(value))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_headers).collect()),
        other => other.clone(),
    }
}

pub(super) fn is_secret_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "api-key",
        "apikey",
        "token",
        "secret",
        "cookie",
    ]
    .iter()
    .any(|needle| name.contains(needle))
}
