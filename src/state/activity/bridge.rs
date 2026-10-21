use super::*;

impl AppState {
    pub(crate) fn activity_start(&self, request_id: &str, endpoint: &str) {
        let _ = self.activity.start(request_id, endpoint);
    }

    pub(crate) fn activity_describe(&self, request_id: &str, description: ActivityDescription) {
        let _ = self.activity.describe(request_id, description);
    }

    pub(crate) fn activity_set_route(
        &self,
        request_id: &str,
        route_id: Option<i64>,
        route_name: Option<&str>,
        strategy: &str,
        candidates: Vec<ActivityCandidate>,
    ) {
        let _ = self
            .activity
            .set_route(request_id, route_id, route_name, strategy, candidates);
    }

    pub(crate) fn activity_waiting(&self, request_id: &str, reason: &str) {
        let _ = self.activity.waiting(request_id, reason);
    }

    pub(crate) fn activity_begin_attempt(&self, request_id: &str, attempt: ActivityAttemptStart) {
        let _ = self.activity.begin_attempt(request_id, attempt);
    }

    pub(crate) fn activity_finish_attempt(&self, request_id: &str, finish: ActivityAttemptFinish) {
        let _ = self.activity.finish_attempt(request_id, finish);
    }

    pub(crate) fn activity_first_token(&self, request_id: &str, first_token_ms: i64) {
        let _ = self.activity.first_token(request_id, first_token_ms);
    }

    pub(crate) fn activity_finish(&self, request_id: &str, finish: ActivityFinish) {
        let _ = self.activity.finish(request_id, finish);
    }

    pub(crate) fn activity_fail(
        &self,
        request_id: &str,
        status_code: Option<i64>,
        error_code: String,
    ) {
        let _ = self.activity.fail(request_id, status_code, error_code);
    }

    pub(crate) fn activity_cancel(&self, request_id: &str, reason: &str) {
        let _ = self.activity.cancel(request_id, reason);
    }
}
