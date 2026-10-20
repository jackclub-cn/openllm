use super::*;
impl MetricsSnapshot {
    pub(super) fn append_gateway(&self, state: &AppState, body: &mut String) {
        let lifetime = &self.lifetime;
        let in_flight = self.in_flight;
        metric_header(body, "openllm_info", "Build information");
        push_metric(
            body,
            "openllm_info",
            &[("version", env!("CARGO_PKG_VERSION"))],
            1,
        );
        metric_header(
            body,
            "openllm_ready",
            "One while the gateway accepts new requests; zero once it is draining",
        );
        push_metric(
            body,
            "openllm_ready",
            &[],
            u8::from(
                !state
                    .shutting_down
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
        );
        metric_header(
            body,
            "openllm_start_time_seconds",
            "Unix timestamp when the process started",
        );
        push_metric(body, "openllm_start_time_seconds", &[], state.started_unix);
        metric_header(
            body,
            "openllm_uptime_seconds",
            "Seconds since the process started",
        );
        push_metric(
            body,
            "openllm_uptime_seconds",
            &[],
            state.started_at.elapsed().as_secs() as i64,
        );
        metric_header(
            body,
            "openllm_requests_total",
            "Completed requests retained by the gateway",
        );
        push_metric(body, "openllm_requests_total", &[], lifetime.requests);
        metric_header(
            body,
            "openllm_requests_successful_total",
            "Completed requests that reached the upstream successfully",
        );
        push_metric(
            body,
            "openllm_requests_successful_total",
            &[],
            lifetime.successful_requests,
        );
        metric_header(
            body,
            "openllm_requests_failed_total",
            "Completed requests that failed",
        );
        push_metric(
            body,
            "openllm_requests_failed_total",
            &[],
            (lifetime.requests - lifetime.successful_requests).max(0),
        );
        metric_header(
            body,
            "openllm_requests_in_flight",
            "Requests currently being processed",
        );
        push_metric(body, "openllm_requests_in_flight", &[], in_flight);
        metric_header(
            body,
            "openllm_request_capacity_limit",
            "Configured global concurrent request cap; zero means unlimited",
        );
        push_metric(
            body,
            "openllm_request_capacity_limit",
            &[],
            state.request_capacity_limit as i64,
        );
        metric_header(
            body,
            "openllm_request_capacity_available",
            "Global admission slots still free; zero when capped or unlimited",
        );
        let available = state
            .request_capacity
            .as_ref()
            .map(|semaphore| semaphore.available_permits() as i64)
            .unwrap_or(0);
        push_metric(body, "openllm_request_capacity_available", &[], available);
        metric_header(
            body,
            "openllm_requests_shed_total",
            "Requests refused by the global admission cap since startup",
        );
        push_metric(
            body,
            "openllm_requests_shed_total",
            &[],
            state
                .requests_shed
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
        );
        metric_header(
            body,
            "openllm_inflight_request_bytes_limit",
            "Configured in-flight request-body budget in bytes; zero means unlimited",
        );
        push_metric(
            body,
            "openllm_inflight_request_bytes_limit",
            &[],
            state.request_bytes_limit as i64,
        );
        metric_header(
            body,
            "openllm_inflight_request_bytes",
            "Request-body bytes currently held by in-flight requests",
        );
        let inflight_request_bytes = state
            .inflight_request_bytes
            .as_ref()
            .map(|budget| budget.used() as i64)
            .unwrap_or(0);
        push_metric(
            body,
            "openllm_inflight_request_bytes",
            &[],
            inflight_request_bytes,
        );
        metric_header(
            body,
            "openllm_request_bytes_shed_total",
            "Requests refused by the in-flight request-byte budget since startup",
        );
        push_metric(
            body,
            "openllm_request_bytes_shed_total",
            &[],
            state
                .request_bytes_shed
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
        );
        metric_header(
            body,
            "openllm_memory_limit_bytes",
            "Configured process-memory ceiling for the pressure guard; zero means disabled",
        );
        push_metric(
            body,
            "openllm_memory_limit_bytes",
            &[],
            state
                .memory_pressure
                .as_ref()
                .map(|pressure| pressure.limit_bytes() as i64)
                .unwrap_or(0),
        );
        metric_header(
            body,
            "openllm_memory_threshold_bytes",
            "Memory usage at which new requests are shed; zero when disabled",
        );
        push_metric(
            body,
            "openllm_memory_threshold_bytes",
            &[],
            state
                .memory_pressure
                .as_ref()
                .map(|pressure| pressure.threshold_bytes() as i64)
                .unwrap_or(0),
        );
        metric_header(
            body,
            "openllm_memory_used_bytes",
            "Latest sampled process memory usage; zero when unavailable",
        );
        let memory_used = state
            .memory_pressure
            .as_ref()
            .and_then(|pressure| pressure.used_bytes())
            .unwrap_or(0) as i64;
        push_metric(body, "openllm_memory_used_bytes", &[], memory_used);
        metric_header(
            body,
            "openllm_memory_pressure",
            "One while new requests are shed under memory pressure",
        );
        let memory_pressure_active = state
            .memory_pressure
            .as_ref()
            .is_some_and(|pressure| pressure.is_shedding())
            as i64;
        push_metric(body, "openllm_memory_pressure", &[], memory_pressure_active);
        metric_header(
            body,
            "openllm_memory_shed_total",
            "Requests shed by the memory-pressure guard since startup",
        );
        push_metric(
            body,
            "openllm_memory_shed_total",
            &[],
            state.memory_shed.load(std::sync::atomic::Ordering::Relaxed) as i64,
        );
        metric_header(
            body,
            "openllm_tokens_total",
            "Retained token counts by type",
        );
        for (kind, value) in [
            ("total", lifetime.tokens),
            ("prompt", lifetime.prompt_tokens),
            ("completion", lifetime.completion_tokens),
            ("cache_read", lifetime.cache_read_tokens),
            ("cache_write", lifetime.cache_write_tokens),
        ] {
            push_metric(body, "openllm_tokens_total", &[("type", kind)], value);
        }
        metric_header(
            body,
            "openllm_estimated_cost_usd_total",
            "Estimated request cost in USD",
        );
        writeln!(
            body,
            "openllm_estimated_cost_usd_total {:.6}",
            lifetime.cost_micros as f64 / 1_000_000.0
        )
        .expect("writing to a String cannot fail");
        metric_header(
            body,
            "openllm_unpriced_requests_total",
            "Completed requests without known pricing",
        );
        push_metric(
            body,
            "openllm_unpriced_requests_total",
            &[],
            lifetime.unpriced_requests,
        );
        metric_header(
            body,
            "openllm_average_latency_ms",
            "Average completed request latency in milliseconds",
        );
        let avg_latency = if lifetime.requests > 0 {
            lifetime.latency_ms_sum as f64 / lifetime.requests as f64
        } else {
            0.0
        };
        writeln!(body, "openllm_average_latency_ms {avg_latency:.3}")
            .expect("writing to a String cannot fail");
    }
}
