use super::*;

impl RequestActivityStore {
    pub fn begin_attempt(
        &self,
        request_id: &str,
        attempt: ActivityAttemptStart,
    ) -> Option<ActivityEvent> {
        self.update(request_id, "attempt_started", |record| {
            if attempt.attempt_index <= record.snapshot.attempt_count {
                return false;
            }
            record.transition("waiting");
            record.attempt_started = Some(Instant::now());
            record.upstream_started = None;
            record.input_reported = false;
            record.cache_reported = false;
            record.snapshot.prompt_tokens = None;
            record.snapshot.completion_tokens = None;
            record.snapshot.cache = ActivityCache {
                state: "unknown".to_string(),
                ..Default::default()
            };
            record.snapshot.waiting_reason = None;
            record.snapshot.context = record.initial_context.clone();
            let mut target = attempt.target;
            target.provider_name = activity_text(&target.provider_name);
            target.upstream_model = activity_text(&target.upstream_model);
            if let Some(context) = record.snapshot.context.as_mut() {
                context.window_tokens = target.context_window_tokens;
                context.input_limit = target.input_limit;
                context.window_source = Some("target".to_string());
            }
            record.snapshot.target = Some(target.clone());
            record.snapshot.attempt_count = attempt.attempt_index;
            // Keep the latest attempts, including the final successful target.
            if record.snapshot.attempts.len() == MAX_ACTIVITY_ATTEMPTS {
                record.snapshot.attempts.remove(0);
                record.snapshot.attempts_truncated = true;
            }
            record.snapshot.attempts.push(ActivityAttempt {
                attempt_index: attempt.attempt_index,
                round: attempt.round,
                candidate_order: attempt.candidate_order,
                provider_id: Some(target.provider_id),
                provider_name: Some(target.provider_name),
                provider_api_key_id: target.provider_api_key_id,
                upstream_model: Some(target.upstream_model),
                started_at: Utc::now().to_rfc3339(),
                latency_ms: 0,
                upstream_headers_ms: None,
                status_code: None,
                success: false,
                reason_code: None,
                failure_scope: None,
            });
            record.snapshot.fallback_count = attempt.attempt_index.saturating_sub(1);
            true
        })
    }

    pub fn finish_attempt(
        &self,
        request_id: &str,
        finish: ActivityAttemptFinish,
    ) -> Option<ActivityEvent> {
        self.update_record(request_id, "attempt_finished", true, |record| {
            let Some(attempt) = record.snapshot.attempts.iter_mut().find(|attempt| {
                attempt.attempt_index == finish.attempt_index && attempt.reason_code.is_none()
            }) else {
                return false;
            };
            attempt.latency_ms = finish.latency_ms.max(0);
            attempt.status_code = finish.status_code;
            attempt.success = finish.success;
            attempt.reason_code = Some(activity_text(&finish.reason_code));
            attempt.failure_scope = finish.failure_scope.map(|value| activity_text(&value));
            if !finish.success && record.finished_instant.is_none() {
                record.snapshot.error_code = Some(activity_text(&finish.reason_code));
                record.transition("routing");
            }
            true
        })
    }

    pub fn upstream_started(&self, request_id: &str) -> Option<ActivityEvent> {
        self.update(request_id, "updated", |record| {
            record.upstream_started = Some(Instant::now());
            record.transition("attempting");
            true
        })
    }

    pub fn headers_received(&self, request_id: &str) -> Option<ActivityEvent> {
        self.update(request_id, "updated", |record| {
            if record.snapshot.phase != "attempting" {
                return false;
            }
            if let Some(started) = record.upstream_started
                && let Some(attempt) = record.snapshot.attempts.last_mut()
            {
                attempt.upstream_headers_ms = Some(elapsed_ms(started, Instant::now()));
            }
            record.transition("headers_received");
            true
        })
    }
}
