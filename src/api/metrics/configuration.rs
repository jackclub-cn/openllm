use super::*;
impl MetricsSnapshot {
    pub(super) fn append_configuration(&self, body: &mut String) {
        let statuses = &self.statuses;
        let affinity_stats = &self.affinity_stats;
        metric_header(
            body,
            "openllm_providers",
            "Provider count by lifecycle and health state",
        );
        for (state, value) in [
            ("healthy", statuses.providers_healthy),
            ("failed", statuses.providers_failed),
            ("untested", statuses.providers_untested),
            ("disabled", statuses.providers_disabled),
        ] {
            push_metric(body, "openllm_providers", &[("state", state)], value);
        }
        metric_header(
            body,
            "openllm_provider_keys",
            "Provider credential count by lifecycle and health state",
        );
        for (state, value) in [
            ("enabled", statuses.provider_keys_enabled),
            ("disabled", statuses.provider_keys_disabled),
            ("healthy", statuses.provider_keys_healthy),
            ("failed", statuses.provider_keys_failed),
            ("untested", statuses.provider_keys_untested),
            ("runtime_error", statuses.provider_keys_runtime_error),
        ] {
            push_metric(body, "openllm_provider_keys", &[("state", state)], value);
        }
        metric_header(
            body,
            "openllm_models",
            "Provider model count by enabled state",
        );
        for (state, value) in [
            ("enabled", statuses.models_enabled),
            ("disabled", statuses.models_disabled),
        ] {
            push_metric(body, "openllm_models", &[("state", state)], value);
        }
        metric_header(body, "openllm_routes", "Route count by enabled state");
        for (state, value) in [
            ("enabled", statuses.routes_enabled),
            ("disabled", statuses.routes_disabled),
        ] {
            push_metric(body, "openllm_routes", &[("state", state)], value);
        }
        metric_header(
            body,
            "openllm_api_keys",
            "Gateway API key count by enabled state",
        );
        for (state, value) in [
            ("enabled", statuses.api_keys_enabled),
            ("disabled", statuses.api_keys_disabled),
        ] {
            push_metric(body, "openllm_api_keys", &[("state", state)], value);
        }
        metric_header(
            body,
            "openllm_audit_logs",
            "Retained configuration change audit records",
        );
        push_metric(body, "openllm_audit_logs", &[], statuses.audit_logs);
        metric_header(
            body,
            "openllm_session_affinity_entries",
            "Prompt/session affinity entries resident in process memory",
        );
        push_metric(
            body,
            "openllm_session_affinity_entries",
            &[],
            affinity_stats.entries as i64,
        );
        metric_header(
            body,
            "openllm_session_affinity_hits_total",
            "Requests that found a healthy prompt/session affinity target",
        );
        push_metric(
            body,
            "openllm_session_affinity_hits_total",
            &[],
            affinity_stats.hits as i64,
        );
        metric_header(
            body,
            "openllm_session_affinity_misses_total",
            "Requests that had no usable prompt/session affinity target",
        );
        push_metric(
            body,
            "openllm_session_affinity_misses_total",
            &[],
            affinity_stats.misses as i64,
        );
        metric_header(
            body,
            "openllm_session_affinity_rebinds_total",
            "Affinity targets replaced because the previous target stopped being usable",
        );
        push_metric(
            body,
            "openllm_session_affinity_rebinds_total",
            &[],
            affinity_stats.rebinds as i64,
        );
        metric_header(
            body,
            "openllm_session_affinity_evictions_total",
            "Affinity entries evicted by expiry or the in-memory capacity bound",
        );
        push_metric(
            body,
            "openllm_session_affinity_evictions_total",
            &[],
            affinity_stats.evictions as i64,
        );
        metric_header(
            body,
            "openllm_session_affinity_persistence_errors_total",
            "Failed reads or writes of the optional SQLite affinity mirror",
        );
        push_metric(
            body,
            "openllm_session_affinity_persistence_errors_total",
            &[],
            affinity_stats.persistence_errors as i64,
        );
    }
}
