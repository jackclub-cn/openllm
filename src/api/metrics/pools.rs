use super::*;
impl MetricsSnapshot {
    pub(super) fn append_pools(&self, body: &mut String) {
        let pool_usage = &self.pool_usage;
        metric_header(
            body,
            "openllm_pool_requests_window",
            "Upstream attempts charged to a provider quota pool inside its rolling window",
        );
        metric_header(
            body,
            "openllm_pool_request_limit",
            "Configured request limit of a provider quota pool; zero means unlimited",
        );
        metric_header(
            body,
            "openllm_pool_tokens_window",
            "Tokens charged to a provider quota pool inside its rolling window",
        );
        metric_header(
            body,
            "openllm_pool_token_limit",
            "Configured token limit of a provider quota pool; zero means unlimited",
        );
        metric_header(
            body,
            "openllm_pool_saturated",
            "Whether a provider quota pool is at or above its saturation threshold",
        );
        metric_header(
            body,
            "openllm_pool_denials_total",
            "Requests refused by a provider quota pool since startup",
        );
        metric_header(
            body,
            "openllm_pool_key_requests_window",
            "Upstream attempts an API key has used from its fair share in the current window",
        );
        metric_header(
            body,
            "openllm_pool_key_request_share",
            "Fair-share request allowance reserved for an API key; zero means unspecified",
        );
        metric_header(
            body,
            "openllm_pool_key_tokens_window",
            "Tokens an API key has used from its fair share in the current window",
        );
        metric_header(
            body,
            "openllm_pool_key_token_share",
            "Fair-share token allowance reserved for an API key; zero means unspecified",
        );
        for pool in pool_usage {
            let provider_id = pool.provider_id.to_string();
            let label = [
                ("pool", pool.name.as_str()),
                ("provider_id", provider_id.as_str()),
            ];
            push_metric(body, "openllm_pool_requests_window", &label, pool.requests);
            push_metric(
                body,
                "openllm_pool_request_limit",
                &label,
                pool.request_limit.unwrap_or(0),
            );
            push_metric(body, "openllm_pool_tokens_window", &label, pool.tokens);
            push_metric(
                body,
                "openllm_pool_token_limit",
                &label,
                pool.token_limit.unwrap_or(0),
            );
            push_metric(body, "openllm_pool_saturated", &label, pool.saturated as u8);
            push_metric(body, "openllm_pool_denials_total", &label, pool.denials);
            for allocation in &pool.allocations {
                let key_id = allocation.api_key_id.to_string();
                let key_label = [
                    ("pool", pool.name.as_str()),
                    ("provider_id", provider_id.as_str()),
                    ("key_id", key_id.as_str()),
                ];
                push_metric(
                    body,
                    "openllm_pool_key_requests_window",
                    &key_label,
                    allocation.key_requests,
                );
                push_metric(
                    body,
                    "openllm_pool_key_request_share",
                    &key_label,
                    allocation.request_share.unwrap_or(0),
                );
                push_metric(
                    body,
                    "openllm_pool_key_tokens_window",
                    &key_label,
                    allocation.key_tokens,
                );
                push_metric(
                    body,
                    "openllm_pool_key_token_share",
                    &key_label,
                    allocation.token_share.unwrap_or(0),
                );
            }
        }
    }
}
