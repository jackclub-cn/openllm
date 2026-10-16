use axum::http::StatusCode;

use crate::models::RouteTarget;

/// The object a failed upstream attempt says is unhealthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureScope {
    Transport,
    Provider,
    Credential,
    Model,
    Endpoint,
    RequestShape,
    RequestContent,
    Client,
}

impl FailureScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Provider => "provider",
            Self::Credential => "credential",
            Self::Model => "model",
            Self::Endpoint => "endpoint",
            Self::RequestShape => "request_shape",
            Self::RequestContent => "request_content",
            Self::Client => "client",
        }
    }
}

/// Whether the gateway should retry, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryDecision {
    DoNotRetry,
    NextTarget,
    /// Try another target immediately, but keep this one eligible for a later
    /// bounded retry instead of resting it as unhealthy.
    WaitThenRetry,
}

impl RetryDecision {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DoNotRetry => "do_not_retry",
            Self::NextTarget => "next_target",
            Self::WaitThenRetry => "wait_then_retry",
        }
    }
}

/// How strongly the response supports the classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureConfidence {
    Exact,
    Inferred,
    Unknown,
}

impl FailureConfidence {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }
}

/// Which local state, if any, should be rested after an upstream failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestAction {
    None,
    Target,
    Credential,
    TargetAndCredential,
}

/// A stable classification of one failed upstream attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UpstreamFailure {
    pub scope: FailureScope,
    pub retry: RetryDecision,
    pub confidence: FailureConfidence,
    pub rest: RestAction,
    pub reason_code: &'static str,
}

impl UpstreamFailure {
    pub(crate) fn should_retry_next_target(self) -> bool {
        matches!(
            self.retry,
            RetryDecision::NextTarget | RetryDecision::WaitThenRetry
        )
    }

    pub(crate) fn should_sink(self) -> bool {
        self.retry == RetryDecision::WaitThenRetry
    }

    pub(crate) fn should_rest_target(self) -> bool {
        matches!(
            self.rest,
            RestAction::Target | RestAction::TargetAndCredential
        )
    }

    pub(crate) fn should_rest_credential(self) -> bool {
        matches!(
            self.rest,
            RestAction::Credential | RestAction::TargetAndCredential
        )
    }

    pub(crate) fn metadata(self) -> crate::state::FailureMetadata {
        crate::state::FailureMetadata {
            scope: self.scope.as_str(),
            retry: self.retry.as_str(),
            confidence: self.confidence.as_str(),
            reason_code: self.reason_code,
            observed_at: std::time::Instant::now(),
        }
    }

    fn target(scope: FailureScope, reason_code: &'static str) -> Self {
        Self {
            scope,
            retry: RetryDecision::NextTarget,
            confidence: FailureConfidence::Inferred,
            rest: RestAction::Target,
            reason_code,
        }
    }

    pub(crate) fn transport(reason_code: &'static str) -> Self {
        Self::target(FailureScope::Transport, reason_code)
    }

    fn credential(target: &RouteTarget, reason_code: &'static str) -> Self {
        Self {
            scope: FailureScope::Credential,
            retry: if target.auth_retryable {
                RetryDecision::NextTarget
            } else {
                RetryDecision::DoNotRetry
            },
            confidence: FailureConfidence::Inferred,
            rest: RestAction::Credential,
            reason_code,
        }
    }

    fn request(
        scope: FailureScope,
        reason_code: &'static str,
        retry: RetryDecision,
        confidence: FailureConfidence,
    ) -> Self {
        Self {
            scope,
            retry,
            confidence,
            rest: RestAction::None,
            reason_code,
        }
    }
}

/// Classifies an HTTP error returned by an upstream.
///
/// This is intentionally conservative. A text marker only changes the scope
/// when it identifies a known failure class; everything else falls back to the
/// status-based behavior that shipped before this module.
pub(crate) fn classify_upstream_failure(
    target: &RouteTarget,
    status: StatusCode,
    body: &[u8],
) -> UpstreamFailure {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();

    if is_content_policy_refusal(&text) {
        return UpstreamFailure::request(
            FailureScope::RequestContent,
            "content_policy_refusal",
            RetryDecision::NextTarget,
            FailureConfidence::Exact,
        );
    }

    if super::compat_thinking_history_error(status, body)
        || (status == StatusCode::BAD_REQUEST && super::upstream_rejects_tool_search(body))
    {
        return UpstreamFailure::request(
            FailureScope::RequestShape,
            "request_shape_incompatible",
            RetryDecision::NextTarget,
            FailureConfidence::Exact,
        );
    }

    if is_context_limit_error(&text) {
        return UpstreamFailure::request(
            FailureScope::Model,
            "context_limit_exceeded",
            RetryDecision::NextTarget,
            FailureConfidence::Exact,
        );
    }

    if is_model_not_found_error(status, &text) {
        return UpstreamFailure::request(
            FailureScope::Model,
            "model_not_found",
            RetryDecision::NextTarget,
            FailureConfidence::Exact,
        );
    }

    if status == StatusCode::TOO_MANY_REQUESTS {
        let quota_exhausted = quota_exhaustion_marker(&text);
        return UpstreamFailure {
            scope: FailureScope::Credential,
            confidence: if quota_exhausted {
                FailureConfidence::Exact
            } else {
                FailureConfidence::Inferred
            },
            rest: if quota_exhausted {
                RestAction::TargetAndCredential
            } else {
                RestAction::None
            },
            retry: if quota_exhausted {
                RetryDecision::NextTarget
            } else {
                RetryDecision::WaitThenRetry
            },
            reason_code: if quota_exhausted {
                "quota_exhausted"
            } else {
                "rate_limited"
            },
        };
    }

    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return UpstreamFailure::credential(target, "credential_rejected");
    }

    if matches!(status, StatusCode::REQUEST_TIMEOUT) || status.as_u16() == 425 {
        return UpstreamFailure::target(FailureScope::Transport, "upstream_timeout");
    }

    if status.is_server_error() {
        return UpstreamFailure::target(FailureScope::Provider, "upstream_unavailable");
    }

    if status == StatusCode::CONFLICT {
        return UpstreamFailure::request(
            FailureScope::RequestContent,
            "request_conflict",
            RetryDecision::NextTarget,
            FailureConfidence::Inferred,
        );
    }

    if matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return UpstreamFailure::request(
            FailureScope::RequestShape,
            "invalid_request",
            RetryDecision::DoNotRetry,
            FailureConfidence::Inferred,
        );
    }

    if status == StatusCode::NOT_FOUND {
        return UpstreamFailure::request(
            FailureScope::Endpoint,
            "endpoint_or_model_not_found",
            RetryDecision::DoNotRetry,
            FailureConfidence::Inferred,
        );
    }

    UpstreamFailure::request(
        FailureScope::Client,
        "unsupported_upstream_error",
        RetryDecision::DoNotRetry,
        FailureConfidence::Unknown,
    )
}

pub(crate) fn quota_exhaustion_marker(text: &str) -> bool {
    [
        "insufficient_quota",
        "insufficient quota",
        "quota exceeded",
        "quota exhausted",
        "exceeded your current quota",
        "out of credits",
        "no credits remaining",
        "insufficient credits",
        "insufficient balance",
        "insufficient_balance",
        "credit balance is too low",
        "credits exhausted",
        "billing hard limit",
        "monthly spending limit",
        "monthly quota",
        "额度不足",
        "余额不足",
        "配额不足",
        "额度已用完",
        "额度用尽",
        "欠费",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn is_content_policy_refusal(text: &str) -> bool {
    [
        "considered high risk",
        "content policy",
        "content_policy",
        "safety policy",
        "safety_policy",
        "rejected because it was considered",
        "blocked by policy",
        "blocked_by_policy",
        "moderation_blocked",
        "content filter",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn is_context_limit_error(text: &str) -> bool {
    [
        "context_length_exceeded",
        "context length exceeded",
        "maximum context length",
        "this model's maximum context",
        "prompt is too long",
        "input is too long",
        "too many tokens",
        "maximum number of tokens",
        "reduce the length of the messages",
        "上下文长度",
        "超过最大上下文",
        "输入过长",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn is_model_not_found_error(status: StatusCode, text: &str) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }
    [
        "model_not_found",
        "model not found",
        "unknown model",
        "does not exist",
        "no such model",
        "模型不存在",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}
