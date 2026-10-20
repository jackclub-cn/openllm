use super::*;
impl MetricsSnapshot {
    pub(super) fn append_runtime(&self, state: &AppState, body: &mut String) {
        let page_count = self.page_count;
        let page_size = self.page_size;
        let free_pages = self.free_pages;
        metric_header(
            body,
            "openllm_database_size_bytes",
            "SQLite database size in bytes including free pages",
        );
        push_metric(
            body,
            "openllm_database_size_bytes",
            &[],
            page_count.saturating_mul(page_size),
        );
        metric_header(
            body,
            "openllm_database_free_bytes",
            "Reusable free space in the SQLite database",
        );
        push_metric(
            body,
            "openllm_database_free_bytes",
            &[],
            free_pages.saturating_mul(page_size),
        );
        metric_header(
            body,
            "openllm_db_pool_connections",
            "Open SQLite pool connections",
        );
        push_metric(
            body,
            "openllm_db_pool_connections",
            &[],
            state.pool.size() as i64,
        );
        metric_header(body, "openllm_db_pool_idle", "Idle SQLite pool connections");
        push_metric(
            body,
            "openllm_db_pool_idle",
            &[],
            state.pool.num_idle() as i64,
        );
        metric_header(
            body,
            "openllm_db_pool_max_connections",
            "Configured SQLite pool size",
        );
        push_metric(
            body,
            "openllm_db_pool_max_connections",
            &[],
            crate::db::db_max_connections() as i64,
        );
        metric_header(
            body,
            "openllm_runtime_lag_millis",
            "How late the async runtime ran its last 1s timer; a large value means a blocked runtime",
        );
        push_metric(
            body,
            "openllm_runtime_lag_millis",
            &[],
            state
                .runtime_lag_millis
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
        );
        metric_header(
            body,
            "openllm_maintenance_runs_total",
            "Completed maintenance cycles since startup",
        );
        push_metric(
            body,
            "openllm_maintenance_runs_total",
            &[],
            state
                .maintenance_runs
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
        );
        metric_header(
            body,
            "openllm_maintenance_last_run_timestamp_seconds",
            "Unix time of the last completed maintenance cycle; zero means none yet",
        );
        push_metric(
            body,
            "openllm_maintenance_last_run_timestamp_seconds",
            &[],
            state
                .maintenance_last_run_unix
                .load(std::sync::atomic::Ordering::Relaxed) as i64,
        );
    }
}
