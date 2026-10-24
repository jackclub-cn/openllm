use super::*;

/// Tracks routing until response headers are ready. Stream translators own the
/// activity heartbeat after that; dropping this future means routing was
/// interrupted before it could write a final usage result.
pub(crate) async fn track_usage_request<F>(
    state: &AppState,
    request_id: &str,
    started: Instant,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    let heartbeat_state = state.clone();
    let heartbeat_request_id = request_id.to_string();
    let heartbeat = tokio::spawn(async move {
        let mut heartbeat = UsageHeartbeat::new(heartbeat_state, heartbeat_request_id);
        loop {
            heartbeat.touch().await;
            tokio::time::sleep(USAGE_HEARTBEAT_INTERVAL).await;
        }
    });
    let mut guard = RoutingUsageGuard {
        state: state.clone(),
        request_id: request_id.to_string(),
        started,
        heartbeat,
        finished: false,
    };
    let result = future.await;
    guard.finished = true;
    result
}

struct RoutingUsageGuard {
    state: AppState,
    request_id: String,
    started: Instant,
    heartbeat: tokio::task::JoinHandle<()>,
    finished: bool,
}

impl Drop for RoutingUsageGuard {
    fn drop(&mut self) {
        self.heartbeat.abort();
        if self.finished {
            return;
        }
        self.state
            .activity_cancel(&self.request_id, "client_cancelled");
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let Some(capacity) = self.state.try_acquire_background_db_slot() else {
            tracing::debug!(
                request_id = %self.request_id,
                "skipping cancelled-request persistence while database writers are saturated"
            );
            return;
        };
        let state = self.state.clone();
        let request_id = self.request_id.clone();
        let latency_ms = self.started.elapsed().as_millis() as i64;
        let performance_json = activity_performance_json(&state, &request_id);
        runtime.spawn(async move {
            let result = sqlx::query_as::<_, (i64, i64)>(
                "UPDATE usage_logs SET in_flight = 0, status_code = 499, success = 0, \
                     latency_ms = ?, performance_json = COALESCE(?, performance_json), \
                     error_message = 'request was cancelled while waiting for upstream response', \
                     last_activity_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE request_id = ? AND in_flight = 1 RETURNING id, streamed",
            )
            .bind(latency_ms)
            .bind(performance_json)
            .bind(&request_id)
            .fetch_optional(&state.pool)
            .await;
            match result {
                Ok(Some((id, streamed))) => {
                    let _ = state.events.send(crate::state::UsageEvent {
                        id,
                        request_id,
                        success: false,
                        streamed: streamed != 0,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, request_id, "failed to finalize cancelled usage request");
                }
            }
            drop(capacity);
        });
    }
}
