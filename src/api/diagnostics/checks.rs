use super::*;

/// A check that passed and needs no operator attention.
pub(super) const CHECK_PASS: &str = "pass";
/// A check that works but is configured in a way an operator should know about.
pub(super) const CHECK_WARN: &str = "warn";
/// A check that will break requests (or already is).
pub(super) const CHECK_FAIL: &str = "fail";

/// How many failed requests a snapshot carries.
pub(super) const MAX_SNAPSHOT_FAILURES: i64 = 20;
/// How many characters of an error/warning message a snapshot keeps. Upstream
/// error bodies can be large and are not worth carrying whole.
pub(super) const MAX_SNAPSHOT_MESSAGE_CHARS: usize = 400;

/// Aggregated result of the operator-facing self-check.
#[derive(Debug, serde::Serialize)]
pub struct SelfCheckReport {
    pub generated_at: String,
    /// `fail` if any check failed, else `warn` if any check warned, else `pass`.
    pub overall: &'static str,
    pub counts: SelfCheckCounts,
    pub checks: Vec<SelfCheckItem>,
}

#[derive(Debug, serde::Serialize)]
pub struct SelfCheckCounts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

/// One structured self-check result.
///
/// Every item answers three questions an on-call operator asks first: what was
/// checked (`title`), what the gateway observed (`detail`), and what to do about
/// it (`hint`). `id` is stable so a runbook can grep for it.
#[derive(Debug, serde::Serialize)]
pub struct SelfCheckItem {
    pub id: &'static str,
    pub category: &'static str,
    pub title: &'static str,
    pub status: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

pub(super) fn check(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    status: &'static str,
    detail: impl Into<String>,
    hint: Option<&'static str>,
) -> SelfCheckItem {
    SelfCheckItem {
        id,
        category,
        title,
        status,
        detail: detail.into(),
        hint,
    }
}

pub(super) fn pass(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    detail: impl Into<String>,
) -> SelfCheckItem {
    check(id, category, title, CHECK_PASS, detail, None)
}

pub(super) fn warn(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    detail: impl Into<String>,
    hint: &'static str,
) -> SelfCheckItem {
    check(id, category, title, CHECK_WARN, detail, Some(hint))
}

pub(super) fn fail(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    detail: impl Into<String>,
    hint: &'static str,
) -> SelfCheckItem {
    check(id, category, title, CHECK_FAIL, detail, Some(hint))
}

/// Runs every check and aggregates the verdict.
///
/// The endpoint never fails: a broken database becomes a failing check instead
/// of an error response, because a support engineer asking "what is wrong?" must
/// still get an answer when the answer is "the database is unreachable".
pub async fn self_check(State(state): State<AppState>) -> Json<SelfCheckReport> {
    Json(run_self_check(&state).await)
}

pub(crate) async fn run_self_check(state: &AppState) -> SelfCheckReport {
    let mut checks = Vec::new();
    checks.push(check_database(state).await);
    checks.push(check_migrations(state).await);
    checks.push(check_instance_lease(state).await);
    checks.push(check_shutdown_config(state).await);
    checks.push(check_circuit_persistence(state).await);
    checks.push(check_provider_health(state).await);
    checks.push(check_provider_credentials(state).await);
    checks.push(check_routes(state).await);
    checks.push(check_gateway_keys(state).await);
    checks.push(check_capacity(state).await);
    checks.push(check_runtime_lag(state).await);
    checks.push(check_maintenance(state).await);
    checks.push(check_webhooks(state).await);

    let mut counts = SelfCheckCounts {
        pass: 0,
        warn: 0,
        fail: 0,
    };
    for item in &checks {
        match item.status {
            CHECK_FAIL => counts.fail += 1,
            CHECK_WARN => counts.warn += 1,
            _ => counts.pass += 1,
        }
    }
    let overall = if counts.fail > 0 {
        CHECK_FAIL
    } else if counts.warn > 0 {
        CHECK_WARN
    } else {
        CHECK_PASS
    };
    SelfCheckReport {
        generated_at: Utc::now().to_rfc3339(),
        overall,
        counts,
        checks,
    }
}
