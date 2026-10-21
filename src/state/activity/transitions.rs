use super::*;

impl RequestActivityStore {
    pub fn describe(
        &self,
        request_id: &str,
        description: ActivityDescription,
    ) -> Option<ActivityEvent> {
        self.update(request_id, "updated", |record| {
            if record.snapshot.phase != "admitted" {
                return false;
            }
            record.transition("preparing");
            record.snapshot.requested_model = Some(activity_text(&description.requested_model));
            record.snapshot.streamed = description.streamed;
            record.snapshot.session_id = description.session_id.map(|value| activity_text(&value));
            record.snapshot.api_key_id = description.api_key_id;
            record.snapshot.api_key_name =
                description.api_key_name.map(|value| activity_text(&value));
            record.snapshot.request_tokens_estimated = description.request_tokens_estimated;
            record.initial_context = description.context.clone();
            record.snapshot.context = description.context;
            true
        })
    }

    pub fn set_route(
        &self,
        request_id: &str,
        route_id: Option<i64>,
        route_name: Option<&str>,
        strategy: &str,
        candidates: Vec<ActivityCandidate>,
    ) -> Option<ActivityEvent> {
        self.update(request_id, "updated", |record| {
            record.transition("routing");
            record.snapshot.route_id = route_id;
            record.snapshot.route_name = route_name.map(activity_text);
            record.snapshot.strategy = Some(activity_text(strategy));
            record.snapshot.candidate_count = candidates.len();
            record.snapshot.candidates_truncated = candidates.len() > MAX_ACTIVITY_CANDIDATES;
            record.snapshot.candidates = candidates
                .into_iter()
                .take(MAX_ACTIVITY_CANDIDATES)
                .map(|mut candidate| {
                    candidate.provider_name =
                        candidate.provider_name.map(|value| activity_text(&value));
                    candidate.upstream_model = activity_text(&candidate.upstream_model);
                    candidate.reason_code = activity_text(&candidate.reason_code);
                    candidate
                })
                .collect();
            record.route_set_instant.get_or_insert_with(Instant::now);
            true
        })
    }

    pub fn waiting(&self, request_id: &str, reason: &str) -> Option<ActivityEvent> {
        self.update(request_id, "updated", |record| {
            let reason = activity_text(reason);
            if record.snapshot.phase == "waiting"
                && record.snapshot.waiting_reason.as_ref() == Some(&reason)
            {
                return false;
            }
            record.transition("waiting");
            record.snapshot.waiting_reason = Some(reason);
            true
        })
    }

    pub fn first_token(&self, request_id: &str, first_token_ms: i64) -> Option<ActivityEvent> {
        self.update(request_id, "first_token", |record| {
            if !record.snapshot.streamed || record.first_token_instant.is_some() {
                return false;
            }
            record.first_token_instant = Some(Instant::now());
            record.transition("streaming");
            record.snapshot.timings.time_to_first_token_ms = Some(first_token_ms.max(0));
            true
        })
    }

    pub fn finish(&self, request_id: &str, finish: ActivityFinish) -> Option<ActivityEvent> {
        let phase = if finish.status_code == 499 {
            "cancelled"
        } else if finish.success {
            "completed"
        } else {
            "failed"
        };
        self.update(request_id, phase, |record| {
            record.transition(phase);
            record.finished_instant = Some(Instant::now());
            record.snapshot.finished_at = Some(Utc::now().to_rfc3339());
            record.snapshot.outcome = if phase == "completed" {
                "success"
            } else {
                phase
            }
            .to_string();
            record.snapshot.waiting_reason = None;
            record.snapshot.status_code = Some(finish.status_code);
            if let Some(code) = finish.error_code {
                record
                    .snapshot
                    .error_code
                    .get_or_insert_with(|| activity_text(&code));
            } else if finish.success {
                record.snapshot.error_code = None;
            }
            record.snapshot.prompt_tokens = Some(finish.usage.prompt_tokens);
            record.snapshot.completion_tokens = Some(finish.usage.completion_tokens);
            record.snapshot.timings.total_ms = finish.latency_ms.max(0);
            if record.snapshot.streamed && record.first_token_instant.is_some() {
                record.snapshot.timings.time_to_first_token_ms = record
                    .snapshot
                    .timings
                    .time_to_first_token_ms
                    .or(finish.first_token_ms);
            }
            record.apply_usage(finish.usage);
            true
        })
    }

    pub fn fail(
        &self,
        request_id: &str,
        status_code: Option<i64>,
        error_code: String,
    ) -> Option<ActivityEvent> {
        self.update(request_id, "failed", |record| {
            record.transition("failed");
            record.finished_instant = Some(Instant::now());
            record.snapshot.finished_at = Some(Utc::now().to_rfc3339());
            record.snapshot.outcome = "failed".to_string();
            record.snapshot.status_code = status_code;
            record.snapshot.error_code = Some(activity_text(&error_code));
            record.snapshot.waiting_reason = None;
            record.snapshot.timings.total_ms = elapsed_ms(record.started_instant, Instant::now());
            true
        })
    }

    pub fn cancel(&self, request_id: &str, reason: &str) -> Option<ActivityEvent> {
        self.update(request_id, "cancelled", |record| {
            record.transition("cancelled");
            record.finished_instant = Some(Instant::now());
            record.snapshot.finished_at = Some(Utc::now().to_rfc3339());
            record.snapshot.outcome = "cancelled".to_string();
            record.snapshot.status_code = Some(499);
            record.snapshot.error_code = Some(activity_text(reason));
            record.snapshot.waiting_reason = None;
            record.snapshot.timings.total_ms = elapsed_ms(record.started_instant, Instant::now());
            true
        })
    }
}
