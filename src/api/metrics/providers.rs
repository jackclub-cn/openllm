use super::*;
impl MetricsSnapshot {
    pub(super) fn append_providers(&self, body: &mut String) {
        let providers = &self.providers;
        let provider_models = &self.provider_models;
        let provider_keys = &self.provider_keys;
        let cooling = &self.cooling;
        let failure_streaks = &self.failure_streaks;
        let circuit_trips = &self.circuit_trips;
        let failure_classifications = &self.failure_classifications;
        let provider_open_threshold = self.provider_open_threshold;
        let concurrency_available = &self.concurrency_available;
        let model_cooling = &self.model_cooling;
        let provider_key_cooling = &self.provider_key_cooling;
        let model_sinks = &self.model_sinks;
        let provider_key_sink_counts = &self.provider_key_sink_counts;
        let model_concurrency_available = &self.model_concurrency_available;
        let provider_key_concurrency_available = &self.provider_key_concurrency_available;
        let provider_key_rpm = &self.provider_key_rpm;
        metric_header(
            body,
            "openllm_provider_health",
            "Current provider health as a one-hot status label",
        );
        metric_header(
            body,
            "openllm_provider_concurrency_limit",
            "Configured per-provider upstream concurrency limit; zero means unlimited",
        );
        metric_header(
            body,
            "openllm_provider_inflight",
            "Current upstream requests holding a per-provider concurrency slot",
        );
        metric_header(
            body,
            "openllm_provider_cooling",
            "Whether a provider is currently excluded from routing by a runtime cooldown",
        );
        metric_header(
            body,
            "openllm_provider_circuit_state",
            "Per-provider circuit breaker state as a one-hot label (closed, open, half_open)",
        );
        metric_header(
            body,
            "openllm_provider_failure_streak",
            "Consecutive provider failures counted by the circuit breaker",
        );
        metric_header(
            body,
            "openllm_provider_circuit_trips_total",
            "Provider circuit hard-opens (trips) since startup",
        );
        metric_header(
            body,
            "openllm_upstream_failures_total",
            "Classified upstream failures since startup",
        );
        for ((scope, retry, reason), count) in failure_classifications {
            push_metric(
                body,
                "openllm_upstream_failures_total",
                &[
                    ("scope", scope.as_str()),
                    ("retry", retry.as_str()),
                    ("reason", reason.as_str()),
                ],
                *count,
            );
        }
        metric_header(
            body,
            "openllm_provider_models",
            "Enabled upstream models per provider",
        );
        metric_header(
            body,
            "openllm_provider_models_disabled",
            "Disabled upstream models per provider",
        );
        metric_header(
            body,
            "openllm_provider_requests_total",
            "Lifetime upstream requests per provider",
        );
        metric_header(
            body,
            "openllm_provider_successes_total",
            "Lifetime successful upstream requests per provider",
        );
        metric_header(
            body,
            "openllm_provider_prompt_tokens_total",
            "Lifetime prompt tokens attributed to each provider",
        );
        metric_header(
            body,
            "openllm_provider_completion_tokens_total",
            "Lifetime completion tokens attributed to each provider",
        );
        metric_header(
            body,
            "openllm_provider_average_latency_ms",
            "Mean upstream latency per provider over retained requests",
        );
        metric_header(
            body,
            "openllm_provider_last_test_latency_ms",
            "Latency of the most recent provider health probe",
        );
        for provider in providers {
            let state = if provider.enabled == 0 {
                "disabled"
            } else {
                match provider.last_test_ok {
                    Some(1) => "healthy",
                    Some(_) => "failed",
                    None => "untested",
                }
            };
            push_metric(
                body,
                "openllm_provider_health",
                &[("provider", &provider.name), ("state", state)],
                1,
            );
            let label = [("provider", provider.name.as_str())];
            let limit = provider.max_concurrency.unwrap_or(0).max(0);
            let in_flight = concurrency_available
                .get(&provider.id)
                .map(|available| limit.saturating_sub(*available as i64))
                .unwrap_or(0);
            push_metric(body, "openllm_provider_concurrency_limit", &label, limit);
            push_metric(body, "openllm_provider_inflight", &label, in_flight);
            push_metric(
                body,
                "openllm_provider_cooling",
                &label,
                cooling.contains(&provider.id) as u8,
            );
            let streak = failure_streaks.get(&provider.id).copied().unwrap_or(0);
            let circuit_state = crate::state::provider_circuit_state(
                streak,
                cooling.contains(&provider.id),
                provider_open_threshold,
            );
            push_metric(
                body,
                "openllm_provider_circuit_state",
                &[("provider", &provider.name), ("state", circuit_state)],
                1,
            );
            push_metric(body, "openllm_provider_failure_streak", &label, streak);
            push_metric(
                body,
                "openllm_provider_circuit_trips_total",
                &label,
                circuit_trips.get(&provider.id).copied().unwrap_or(0),
            );
            push_metric(
                body,
                "openllm_provider_models",
                &label,
                provider.models_enabled,
            );
            push_metric(
                body,
                "openllm_provider_models_disabled",
                &label,
                provider.models_disabled,
            );
            push_metric(
                body,
                "openllm_provider_requests_total",
                &label,
                provider.requests,
            );
            push_metric(
                body,
                "openllm_provider_successes_total",
                &label,
                provider.successes,
            );
            push_metric(
                body,
                "openllm_provider_prompt_tokens_total",
                &label,
                provider.prompt_tokens,
            );
            push_metric(
                body,
                "openllm_provider_completion_tokens_total",
                &label,
                provider.completion_tokens,
            );
            let provider_avg_latency = if provider.requests > 0 {
                provider.latency_ms_sum as f64 / provider.requests as f64
            } else {
                0.0
            };
            writeln!(
                body,
                "openllm_provider_average_latency_ms{{provider=\"{}\"}} {:.3}",
                escape_label(&provider.name),
                provider_avg_latency
            )
            .expect("writing to a String cannot fail");
            if let Some(latency) = provider.last_test_latency_ms {
                push_metric(
                    body,
                    "openllm_provider_last_test_latency_ms",
                    &label,
                    latency,
                );
            }
        }
        metric_header(
            body,
            "openllm_model_cooling",
            "Current model cooldown state",
        );
        metric_header(
            body,
            "openllm_model_concurrency_limit",
            "Configured per-model upstream concurrency limit; zero means unlimited",
        );
        metric_header(
            body,
            "openllm_model_inflight",
            "Current upstream requests holding a per-model concurrency slot",
        );
        metric_header(
            body,
            "openllm_model_sink",
            "Whether a model target is temporarily sinked after a short rate limit",
        );
        for model in provider_models {
            let label = [
                ("provider", model.provider.as_str()),
                ("model", model.model.as_str()),
            ];
            let limit = model.max_concurrency.unwrap_or(0).max(0);
            let in_flight = model_concurrency_available
                .get(&(model.provider_id, model.model.clone()))
                .map(|available| limit.saturating_sub(*available as i64))
                .unwrap_or(0);
            push_metric(
                body,
                "openllm_model_cooling",
                &label,
                model_cooling.contains(&(model.provider_id, model.model.clone())) as u8,
            );
            push_metric(body, "openllm_model_concurrency_limit", &label, limit);
            push_metric(body, "openllm_model_inflight", &label, in_flight);
            push_metric(
                body,
                "openllm_model_sink",
                &label,
                model_sinks.contains(&(model.provider_id, model.model.clone())) as u8,
            );
        }
        metric_header(
            body,
            "openllm_provider_key_cooling",
            "Current provider key cooldown state",
        );
        metric_header(
            body,
            "openllm_provider_key_concurrency_limit",
            "Configured per-credential upstream concurrency limit; zero means unlimited",
        );
        metric_header(
            body,
            "openllm_provider_key_inflight",
            "Current upstream requests holding a per-credential concurrency slot",
        );
        metric_header(
            body,
            "openllm_provider_key_rpm_limit",
            "Configured per-credential rolling 60-second request limit; zero means unlimited",
        );
        metric_header(
            body,
            "openllm_provider_key_rpm_window",
            "Requests started by a provider credential in the current rolling 60-second window",
        );
        metric_header(
            body,
            "openllm_provider_key_sink_count",
            "Model targets currently sinked for a provider credential",
        );
        for key in provider_keys {
            let key_id = key.id.to_string();
            let label = [
                ("provider", key.provider.as_str()),
                ("key", key.name.as_str()),
                ("key_id", key_id.as_str()),
            ];
            let concurrency_limit = key.max_concurrency.unwrap_or(0).max(0);
            let in_flight = provider_key_concurrency_available
                .get(&key.id)
                .map(|available| concurrency_limit.saturating_sub(*available as i64))
                .unwrap_or(0);
            let rpm_limit = key.requests_per_minute.unwrap_or(0).max(0);
            push_metric(
                body,
                "openllm_provider_key_cooling",
                &label,
                provider_key_cooling.contains(&key.id) as u8,
            );
            push_metric(
                body,
                "openllm_provider_key_concurrency_limit",
                &label,
                concurrency_limit,
            );
            push_metric(body, "openllm_provider_key_inflight", &label, in_flight);
            push_metric(body, "openllm_provider_key_rpm_limit", &label, rpm_limit);
            push_metric(
                body,
                "openllm_provider_key_rpm_window",
                &label,
                provider_key_rpm.get(&key.id).copied().unwrap_or(0) as i64,
            );
            push_metric(
                body,
                "openllm_provider_key_sink_count",
                &label,
                provider_key_sink_counts.get(&key.id).copied().unwrap_or(0) as i64,
            );
        }
    }
}
