use super::*;

pub(super) async fn acquire_limit_slot(
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    limit: usize,
    wait: Duration,
    label: String,
    gate_closed_message: &'static str,
) -> AppResult<OwnedSemaphorePermit> {
    if wait.is_zero() {
        return semaphore
            .try_acquire_owned()
            .map_err(|_| AppError::UpstreamStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: format!("{label} is at its concurrency limit ({limit}); retry shortly"),
                retry_after: Some(Duration::from_secs(1)),
            });
    }

    match tokio::time::timeout(wait, semaphore.acquire_owned()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(AppError::Upstream(gate_closed_message.to_string())),
        Err(_) => Err(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "{label} stayed at its concurrency limit ({limit}) for {} seconds",
                wait.as_secs()
            ),
            retry_after: Some(Duration::from_secs(1)),
        }),
    }
}

pub(super) const PROVIDER_KEY_RPM_WINDOW: Duration = Duration::from_secs(60);

pub(super) async fn provider_key_rpm_window(
    state: &AppState,
    provider_api_key_id: i64,
) -> (usize, Option<Duration>) {
    let now = Instant::now();
    let mut windows = state.provider_key_rpm.lock().await;
    let starts = windows.entry(provider_api_key_id).or_default();
    while starts
        .front()
        .is_some_and(|started| now.saturating_duration_since(*started) >= PROVIDER_KEY_RPM_WINDOW)
    {
        starts.pop_front();
    }
    let retry_after = starts.front().map(|started| {
        PROVIDER_KEY_RPM_WINDOW.saturating_sub(now.saturating_duration_since(*started))
    });
    (starts.len(), retry_after)
}

pub(super) async fn check_provider_key_rpm(
    state: &AppState,
    provider_api_key_id: i64,
    limit: usize,
) -> AppResult<()> {
    let (used, retry_after) = provider_key_rpm_window(state, provider_api_key_id).await;
    if used < limit {
        return Ok(());
    }
    Err(AppError::UpstreamStatus {
        status: StatusCode::TOO_MANY_REQUESTS,
        message: format!("provider API key is at its rolling 60-second request limit ({limit})"),
        retry_after: Some(
            retry_after
                .unwrap_or(Duration::from_secs(1))
                .max(Duration::from_millis(1)),
        ),
    })
}

pub(super) async fn record_provider_key_rpm(
    state: &AppState,
    provider_api_key_id: i64,
    limit: usize,
) -> AppResult<()> {
    let now = Instant::now();
    let mut windows = state.provider_key_rpm.lock().await;
    let starts = windows.entry(provider_api_key_id).or_default();
    while starts
        .front()
        .is_some_and(|started| now.saturating_duration_since(*started) >= PROVIDER_KEY_RPM_WINDOW)
    {
        starts.pop_front();
    }
    if starts.len() >= limit {
        let retry_after = starts
            .front()
            .map(|started| {
                PROVIDER_KEY_RPM_WINDOW.saturating_sub(now.saturating_duration_since(*started))
            })
            .unwrap_or(Duration::from_secs(1))
            .max(Duration::from_millis(1));
        return Err(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "provider API key is at its rolling 60-second request limit ({limit})"
            ),
            retry_after: Some(retry_after),
        });
    }
    starts.push_back(now);
    Ok(())
}

pub(super) async fn acquire_provider_key_slot(
    state: &AppState,
    target: &RouteTarget,
) -> Result<Option<OwnedSemaphorePermit>, AcquireSlotError> {
    let Some(provider_api_key_id) = target.provider_api_key_id else {
        return Ok(None);
    };
    let Some(limit) = target
        .provider_key_max_concurrency
        .filter(|limit| *limit > 0)
    else {
        return Ok(None);
    };
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let semaphore = {
        let mut semaphores = state.provider_key_concurrency.lock().await;
        semaphores
            .entry(provider_api_key_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(limit.max(1))))
            .clone()
    };
    let wait = target
        .provider_key_queue_wait_ms
        .map(|milliseconds| Duration::from_millis(milliseconds.unsigned_abs()))
        .unwrap_or(DEFAULT_PROVIDER_QUEUE_TIMEOUT);
    let label = target
        .provider_api_key_name
        .clone()
        .unwrap_or_else(|| format!("provider API key {provider_api_key_id}"));

    if let Ok(permit) = semaphore.clone().try_acquire_owned() {
        return Ok(Some(permit));
    }
    if wait.is_zero() {
        return Err(AcquireSlotError::ProviderApiKey(AppError::UpstreamStatus {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "provider API key {label} is at its concurrency limit ({limit}); retry shortly"
            ),
            retry_after: Some(Duration::from_secs(1)),
        }));
    }

    let _queue_guard = if let Some(queue_limit) = target.provider_key_queue_limit {
        if queue_limit <= 0 {
            return Err(AcquireSlotError::ProviderApiKey(AppError::UpstreamStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: format!(
                    "provider API key {label} is at its concurrency limit ({limit}) and queueing is disabled"
                ),
                retry_after: Some(Duration::from_secs(1)),
            }));
        }
        let queue_limit = usize::try_from(queue_limit).unwrap_or(usize::MAX);
        let queue = {
            let mut queues = state.provider_key_queue.lock().await;
            queues
                .entry(provider_api_key_id)
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(queue_limit)))
                .clone()
        };
        Some(queue.try_acquire_owned().map_err(|_| {
            AcquireSlotError::ProviderApiKey(AppError::UpstreamStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: format!(
                    "provider API key {label} queue is full ({queue_limit}); retry shortly"
                ),
                retry_after: Some(Duration::from_secs(1)),
            })
        })?)
    } else {
        None
    };

    acquire_limit_slot(
        semaphore,
        limit,
        wait,
        format!("provider API key {label}"),
        "provider API key concurrency gate closed",
    )
    .await
    .map(Some)
    .map_err(AcquireSlotError::ProviderApiKey)
}

/// Waits for model and provider concurrency slots. Model slots are acquired
/// first so a queue of requests for one hot model cannot occupy every provider
/// slot and starve healthy sibling models. The selected upstream credential is
/// checked before either provider-wide or model-wide gates.
pub(in crate::proxy) async fn acquire_upstream_slots(
    state: &AppState,
    target: &RouteTarget,
) -> Result<Option<UpstreamSlots>, AcquireSlotError> {
    if let (Some(provider_api_key_id), Some(limit)) = (
        target.provider_api_key_id,
        target
            .provider_key_requests_per_minute
            .filter(|limit| *limit > 0),
    ) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        check_provider_key_rpm(state, provider_api_key_id, limit)
            .await
            .map_err(AcquireSlotError::ProviderApiKey)?;
    }
    let provider_key = acquire_provider_key_slot(state, target).await?;

    let model = match target.model_max_concurrency.filter(|limit| *limit > 0) {
        Some(limit) => {
            let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
            let semaphore = {
                let mut semaphores = state.model_concurrency.lock().await;
                semaphores
                    .entry((target.provider_id, target.upstream_model.clone()))
                    .or_insert_with(|| {
                        std::sync::Arc::new(tokio::sync::Semaphore::new(limit_usize.max(1)))
                    })
                    .clone()
            };
            let wait = target
                .model_queue_timeout_seconds
                .map(|seconds| Duration::from_secs(seconds.unsigned_abs()))
                .unwrap_or(DEFAULT_PROVIDER_QUEUE_TIMEOUT);
            Some(
                acquire_limit_slot(
                    semaphore,
                    limit_usize,
                    wait,
                    format!("{}/{}", target.provider_name, target.upstream_model),
                    "model concurrency gate closed",
                )
                .await
                .map_err(AcquireSlotError::Model)?,
            )
        }
        None => None,
    };

    let provider = match target.max_concurrency.filter(|limit| *limit > 0) {
        Some(limit) => {
            let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
            let semaphore = {
                let mut semaphores = state.provider_concurrency.lock().await;
                semaphores
                    .entry(target.provider_id)
                    .or_insert_with(|| {
                        std::sync::Arc::new(tokio::sync::Semaphore::new(limit_usize.max(1)))
                    })
                    .clone()
            };
            let wait = target
                .queue_timeout_seconds
                .map(|seconds| Duration::from_secs(seconds.unsigned_abs()))
                .unwrap_or(DEFAULT_PROVIDER_QUEUE_TIMEOUT);
            Some(
                acquire_limit_slot(
                    semaphore,
                    limit_usize,
                    wait,
                    target.provider_name.clone(),
                    "provider concurrency gate closed",
                )
                .await
                .map_err(AcquireSlotError::Provider)?,
            )
        }
        None => None,
    };

    if let (Some(provider_api_key_id), Some(limit)) = (
        target.provider_api_key_id,
        target
            .provider_key_requests_per_minute
            .filter(|limit| *limit > 0),
    ) {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        record_provider_key_rpm(state, provider_api_key_id, limit)
            .await
            .map_err(AcquireSlotError::ProviderApiKey)?;
    }

    if provider_key.is_none() && model.is_none() && provider.is_none() {
        Ok(None)
    } else {
        Ok(Some(UpstreamSlots {
            provider_key,
            provider,
            model,
        }))
    }
}
